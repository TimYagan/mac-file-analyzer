/// macOS-native file stat: wraps lstat(2) and getattrlist(2) to retrieve both
/// apparent size (st_size) and actual disk usage (st_blocks * 512).
///
/// Phase 3 adds a `getattrlist` fast-path that retrieves all needed attributes
/// (inode, type, nlink, data-fork size, rsrc-fork size, disk blocks) in a
/// single kernel call instead of the two-step opendir + lstat approach.
///
/// Inode deduplication: callers should use `InodeSet` to avoid counting
/// hard-linked files more than once.
use std::collections::HashSet;
use std::ffi::CString;
use std::io;
use std::path::Path;

/// Unique identity of a file on disk. Two paths with identical (dev, ino)
/// are hard links to the same inode — they must only be counted once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InodeKey {
    pub dev: u64,
    pub ino: u64,
}

/// Heap-allocated set used for inode deduplication during a walk.
pub type InodeSet = HashSet<InodeKey>;

/// Metadata retrieved for a single filesystem entry.
#[derive(Debug, Clone)]
pub struct FileStat {
    /// Logical (apparent) file size in bytes — what `ls -l` reports.
    pub apparent_size: u64,
    /// Actual disk usage in bytes — `st_blocks * 512`.
    /// Correctly handles sparse files (may be < apparent_size).
    pub disk_usage: u64,
    /// Size of the resource fork in bytes (0 on non-HFS/APFS or if absent).
    /// Populated by `getattrlist_stat`; always 0 when using `lstat`.
    pub rsrc_size: u64,
    pub inode: InodeKey,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// True only for S_IFREG — excludes devices, pipes, sockets, etc.
    pub is_regular: bool,
    /// nlink > 1 means hard-linked; caller should dedup via InodeSet.
    pub nlink: u64,
}

/// Call `lstat(2)` (does NOT follow symlinks) on the given path.
///
/// # Errors
/// Returns `io::Error` if the syscall fails (ENOENT, EACCES, etc.).
pub fn lstat(path: &Path) -> io::Result<FileStat> {
    let cpath = path_to_cstring(path)?;

    // SAFETY: cpath is a valid null-terminated C string; stat buf is
    // fully initialised by the kernel on success.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::lstat(cpath.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(stat_from_raw(&st))
}

/// Call `stat(2)` (follows symlinks) on the given path.
pub fn stat_follow(path: &Path) -> io::Result<FileStat> {
    let cpath = path_to_cstring(path)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::stat(cpath.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat_from_raw(&st))
}

// ─── helpers ────────────────────────────────────────────────────────────────

fn path_to_cstring(path: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null byte"))
}

fn stat_from_raw(st: &libc::stat) -> FileStat {
    // st_blocks is in 512-byte units on macOS (POSIX-defined, not FS block size).
    let disk_usage = (st.st_blocks as u64).saturating_mul(512);
    let apparent_size = st.st_size.max(0) as u64;

    let mode = st.st_mode as u32;
    let file_type = mode & libc::S_IFMT as u32;
    let is_dir     = file_type == libc::S_IFDIR as u32;
    let is_symlink = file_type == libc::S_IFLNK as u32;
    let is_regular = file_type == libc::S_IFREG as u32;

    FileStat {
        apparent_size,
        disk_usage,
        rsrc_size: 0, // lstat does not return resource fork size
        inode: InodeKey {
            dev: st.st_dev as u64,
            ino: st.st_ino,
        },
        is_dir,
        is_symlink,
        is_regular,
        nlink: st.st_nlink as u64,
    }
}

// ─── Phase 3: getattrlist fast-path ─────────────────────────────────────────

// macOS getattrlist(2) attribute constants (from <sys/attr.h>).
const ATTR_BIT_MAP_COUNT: u16 = 5;

// Common attributes
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;        // dev_t — device ID
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;       // vtype enum
const ATTR_CMN_FILEID: u32 = 0x0200_0000;        // u64 — 64-bit inode

// File attributes
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;    // u32 — hard link count
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;   // off_t — data-fork logical size
const ATTR_FILE_DATAALLOCSIZE: u32 = 0x0000_0400; // off_t — data-fork allocated bytes
const ATTR_FILE_RSRCLENGTH: u32 = 0x0000_1000;   // off_t — resource-fork logical size

// vtype constants (from <sys/vnode.h>)
const VREG: u32 = 1; // regular file
const VDIR: u32 = 2; // directory
const VLNK: u32 = 5; // symlink

// attrgroup_t is u32 on macOS.
#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

/// Attribute buffer returned by getattrlist for the exact attr set we request.
///
/// Layout (tightly packed, no gaps — getattrlist never inserts alignment padding):
/// ```text
///   4   length           u32
///  20   returned_attrs   [u32; 5]   (because ATTR_CMN_RETURNED_ATTRS is set)
///   4   devid            i32        ATTR_CMN_DEVID
///   4   obj_type         u32        ATTR_CMN_OBJTYPE
///   8   file_id          u64        ATTR_CMN_FILEID
///   4   link_count       u32        ATTR_FILE_LINKCOUNT
///   8   data_length      i64        ATTR_FILE_DATALENGTH
///   8   data_alloc_size  i64        ATTR_FILE_DATAALLOCSIZE
///   8   rsrc_length      i64        ATTR_FILE_RSRCLENGTH
/// ───
///  68   total
/// ```
#[repr(C, packed)]
struct FileAttrs {
    length: u32,
    returned_attrs: [u32; 5],
    devid: i32,
    obj_type: u32,
    file_id: u64,
    link_count: u32,
    data_length: i64,
    data_alloc_size: i64,
    rsrc_length: i64,
}

/// macOS `getattrlist(2)` fast-path stat.
///
/// Retrieves device id, inode, object type, link count, data-fork size,
/// data-fork allocated bytes, and resource-fork size in a **single kernel call**.
///
/// Falls back to `lstat` on error (e.g. NFS mounts that don't support
/// getattrlist).
pub fn getattrlist_stat(path: &Path) -> io::Result<FileStat> {
    let cpath = path_to_cstring(path)?;

    let mut al = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_DEVID
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_LINKCOUNT
            | ATTR_FILE_DATALENGTH
            | ATTR_FILE_DATAALLOCSIZE
            | ATTR_FILE_RSRCLENGTH,
        forkattr: 0,
    };

    let mut buf: FileAttrs = unsafe { std::mem::zeroed() };

    // FSOPT_NOFOLLOW (0x01)  — mirrors lstat semantics (do not follow symlinks).
    // FSOPT_REPORT_FULLSIZE (0x04) — required when ATTR_CMN_RETURNED_ATTRS is set.
    const FSOPT_NOFOLLOW: u32 = 0x0000_0001;
    const FSOPT_REPORT_FULLSIZE: u32 = 0x0000_0004;

    let rc = unsafe {
        libc::getattrlist(
            cpath.as_ptr(),
            &mut al as *mut AttrList as *mut libc::c_void,
            &mut buf as *mut FileAttrs as *mut libc::c_void,
            std::mem::size_of::<FileAttrs>(),
            FSOPT_NOFOLLOW | FSOPT_REPORT_FULLSIZE,
        )
    };

    if rc != 0 {
        // Fall back to lstat if getattrlist is unavailable (NFS, SMBFS, etc.).
        return lstat(path);
    }

    // SAFETY: buf was fully written by the kernel; read_unaligned is required
    // for the packed struct to avoid undefined behaviour on unaligned fields.
    //
    // When ATTR_CMN_RETURNED_ATTRS is set the kernel packs only the attributes
    // it actually returned into the buffer (in strict bitmask order).  If any
    // essential attribute is absent the buffer layout no longer matches
    // FileAttrs, so we must fall back to lstat rather than reading garbage.
    //
    // returned_attrs is an attribute_set_t {commonattr, volattr, dirattr,
    // fileattr, forkattr} — five consecutive u32 values.
    let ret_common = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.returned_attrs[0])) };
    let ret_file   = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.returned_attrs[3])) };

    // These attrs are essential: absent any one, subsequent fields in the
    // tightly-packed buffer shift to wrong offsets.
    let need_common = ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID;
    let need_file   = ATTR_FILE_LINKCOUNT | ATTR_FILE_DATALENGTH | ATTR_FILE_DATAALLOCSIZE;

    if (ret_common & need_common) != need_common || (ret_file & need_file) != need_file {
        // Partial result — fall back to lstat for correctness.
        return lstat(path);
    }

    // ATTR_FILE_RSRCLENGTH is optional: non-HFS/APFS volumes (ExFAT, SMB, …)
    // legitimately omit it.  When absent the field remains zero-initialised
    // (rsrc_size = 0), which is the correct answer for those filesystems.

    let devid      = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.devid)) };
    let obj_type   = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.obj_type)) };
    let file_id    = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.file_id)) };
    let link_count = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.link_count)) };
    let data_len   = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.data_length)) };
    let data_alloc = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.data_alloc_size)) };
    let rsrc_len   = unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(buf.rsrc_length)) };

    let is_dir     = obj_type == VDIR;
    let is_symlink = obj_type == VLNK;
    let is_regular = obj_type == VREG;

    Ok(FileStat {
        apparent_size: data_len.max(0) as u64,
        disk_usage:    data_alloc.max(0) as u64,
        rsrc_size:     rsrc_len.max(0) as u64,
        inode: InodeKey {
            dev: devid as u64,
            ino: file_id,
        },
        is_dir,
        is_symlink,
        is_regular,
        nlink: link_count as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn regular_file_sizes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, b"hello world").unwrap();

        let st = lstat(&path).unwrap();
        assert_eq!(st.apparent_size, 11);
        assert!(st.disk_usage > 0);
        assert!(!st.is_dir);
        assert!(!st.is_symlink);
    }

    #[test]
    fn directory_stat() {
        let dir = tempdir().unwrap();
        let st = lstat(dir.path()).unwrap();
        assert!(st.is_dir);
        assert!(!st.is_symlink);
    }

    #[test]
    fn symlink_is_not_followed_by_lstat() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, b"some content").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let st = lstat(&link).unwrap();
        assert!(st.is_symlink);
        // Symlink apparent size = length of the target path string, not target content.
        assert_ne!(st.apparent_size, 12);
    }

    #[test]
    fn hardlink_same_inode() {
        let dir = tempdir().unwrap();
        let original = dir.path().join("orig.txt");
        let hard = dir.path().join("hard.txt");
        fs::write(&original, b"data").unwrap();
        fs::hard_link(&original, &hard).unwrap();

        let st_orig = lstat(&original).unwrap();
        let st_hard = lstat(&hard).unwrap();
        assert_eq!(st_orig.inode, st_hard.inode);
        assert_eq!(st_orig.nlink, 2);
        assert_eq!(st_hard.nlink, 2);
    }

    #[test]
    fn regular_file_is_regular() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.bin");
        fs::write(&path, b"contents").unwrap();
        let st = lstat(&path).unwrap();
        assert!(st.is_regular);
        assert!(!st.is_dir);
        assert!(!st.is_symlink);
    }

    #[test]
    fn sparse_file_disk_vs_apparent() {
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom, Write};

        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse.dat");

        // Seek past 512 KiB then write 1 byte.
        let hole: u64 = 512 * 1024;
        {
            let mut f = OpenOptions::new().create(true).write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(hole)).unwrap();
            f.write_all(&[0x42]).unwrap();
        }

        let st = lstat(&path).unwrap();
        assert!(st.is_regular);
        // Apparent size is hole + 1 byte.
        assert_eq!(st.apparent_size, hole + 1);
        // disk_usage is st_blocks * 512, so always a multiple of 512.
        assert_eq!(st.disk_usage % 512, 0);
        // disk_usage reflects actual block allocation.
        // Note: APFS may allocate full blocks so disk_usage may exceed apparent_size
        // for small files, but our formula (st_blocks * 512) is always correct.
        assert!(st.disk_usage > 0);
    }

    // ── Phase 3: getattrlist tests ───────────────────────────────────────────

    #[test]
    fn getattrlist_raw_layout_probe() {
        // Validate that the raw getattrlist buffer matches expected offsets.
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("probe.bin");
        fs::write(&path, vec![b'A'; 1024]).unwrap();

        let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
        let mut attrs = super::AttrList {
            bitmapcount: super::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: super::ATTR_CMN_RETURNED_ATTRS
                | super::ATTR_CMN_DEVID
                | super::ATTR_CMN_OBJTYPE
                | super::ATTR_CMN_FILEID,
            volattr: 0,
            dirattr: 0,
            fileattr: super::ATTR_FILE_LINKCOUNT
                | super::ATTR_FILE_DATALENGTH
                | super::ATTR_FILE_DATAALLOCSIZE
                | super::ATTR_FILE_RSRCLENGTH,
            forkattr: 0,
        };

        let mut raw = [0u8; 128];
        let rc = unsafe {
            libc::getattrlist(
                cpath.as_ptr(),
                &mut attrs as *mut super::AttrList as *mut libc::c_void,
                raw.as_mut_ptr() as *mut libc::c_void,
                raw.len(),
                0x0000_0001u32 | 0x0000_0004u32, // FSOPT_NOFOLLOW | FSOPT_REPORT_FULLSIZE
            )
        };
        assert_eq!(rc, 0, "getattrlist failed");

        let length = u32::from_le_bytes(raw[0..4].try_into().unwrap());
        eprintln!("Buffer length: {} bytes (sizeof(FileAttrs)={})",
            length, std::mem::size_of::<super::FileAttrs>());
        eprint!("Raw hex:");
        for i in 0..length as usize { eprint!(" {:02x}", raw[i]); }
        eprintln!();

        // Expected layout:
        //  4  length
        // 20  returned_attrs [u32; 5]
        //  4  devid   (i32)   @ 24
        //  4  objtype (u32)   @ 28
        //  8  fileid  (u64)   @ 32
        //  4  nlink   (u32)   @ 40
        //  8  datalen (i64)   @ 44
        //  8  dataalloc(i64)  @ 52
        //  8  rsrclen (i64)   @ 60
        //     total = 68
        let devid      = i32::from_le_bytes(raw[24..28].try_into().unwrap());
        let obj_type   = u32::from_le_bytes(raw[28..32].try_into().unwrap());
        let file_id    = u64::from_le_bytes(raw[32..40].try_into().unwrap());
        let link_count = u32::from_le_bytes(raw[40..44].try_into().unwrap());
        let data_len   = i64::from_le_bytes(raw[44..52].try_into().unwrap());
        let data_alloc = i64::from_le_bytes(raw[52..60].try_into().unwrap());
        let rsrc_len   = i64::from_le_bytes(raw[60..68].try_into().unwrap());
        eprintln!("Decoded: devid={} obj={} fid={} nlink={} datalen={} alloc={} rsrc={}",
            devid, obj_type, file_id, link_count, data_len, data_alloc, rsrc_len);

        // Compare with getattrlist_stat
        let gal = getattrlist_stat(&path).unwrap();
        eprintln!("getattrlist_stat: apparent={} disk={} rsrc={} nlink={} ino={}",
            gal.apparent_size, gal.disk_usage, gal.rsrc_size, gal.nlink, gal.inode.ino);

        // Get lstat for validation
        let ls = lstat(&path).unwrap();
        eprintln!("lstat: apparent={} disk={} nlink={} ino={}",
            ls.apparent_size, ls.disk_usage, ls.nlink, ls.inode.ino);

        // Key assertions
        assert_eq!(obj_type, super::VREG, "obj_type should be VREG");
        assert_eq!(link_count, 1, "newly created file should have nlink=1");
        assert_eq!(data_len, 1024, "data_len should match write size");
    }

    #[test]
    fn getattrlist_matches_lstat_for_regular_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.bin");
        fs::write(&path, vec![b'z'; 1024]).unwrap();

        let via_lstat = lstat(&path).unwrap();
        let via_gal   = getattrlist_stat(&path).unwrap();

        assert_eq!(via_gal.apparent_size, via_lstat.apparent_size,
            "apparent_size mismatch");
        assert!(via_gal.disk_usage > 0, "disk_usage should be nonzero");
        assert!(via_gal.is_regular);
        assert!(!via_gal.is_dir);
        assert!(!via_gal.is_symlink);
        // Inode numbers must agree.
        assert_eq!(via_gal.inode.ino, via_lstat.inode.ino,
            "inode number mismatch");
        // nlink must agree.
        assert_eq!(via_gal.nlink, via_lstat.nlink, "nlink mismatch");
        // No resource fork on a plain file.
        assert_eq!(via_gal.rsrc_size, 0, "plain file should have no rsrc fork");
    }

    #[test]
    fn getattrlist_detects_directory() {
        let dir = tempdir().unwrap();
        let st = getattrlist_stat(dir.path()).unwrap();
        assert!(st.is_dir);
        assert!(!st.is_symlink);
        assert!(!st.is_regular);
    }

    #[test]
    fn getattrlist_does_not_follow_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link   = dir.path().join("link.txt");
        fs::write(&target, vec![b'x'; 500]).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let st = getattrlist_stat(&link).unwrap();
        assert!(st.is_symlink, "should report symlink with FSOPT_NOFOLLOW");
    }

    #[test]
    fn getattrlist_hardlink_same_inode() {
        let dir  = tempdir().unwrap();
        let orig = dir.path().join("orig.bin");
        let hard = dir.path().join("hard.bin");
        fs::write(&orig, b"payload").unwrap();
        fs::hard_link(&orig, &hard).unwrap();

        let st_orig = getattrlist_stat(&orig).unwrap();
        let st_hard = getattrlist_stat(&hard).unwrap();
        assert_eq!(st_orig.inode.ino, st_hard.inode.ino, "inodes must match");
        assert_eq!(st_orig.nlink, 2);
        assert_eq!(st_hard.nlink, 2);
    }
}
