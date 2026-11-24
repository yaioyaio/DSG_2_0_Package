use super::{is_link_or_reparse_point, FileIdentity, MAX_DIRECTORY_DEPTH};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::mem::size_of;
use std::ops::Deref;
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::null_mut;
use std::sync::Arc;
use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    GetVolumeInformationByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
    FILE_SHARE_READ,
};

pub(crate) struct ObservedMetadata {
    metadata: Metadata,
    pub(super) pin: Arc<File>,
    pub(super) id: FileIdentity,
    pub(super) links: u32,
    // The caller retains the root observation for the full walk.
    ancestors: Vec<File>,
}

impl Deref for ObservedMetadata {
    type Target = Metadata;

    fn deref(&self) -> &Metadata {
        &self.metadata
    }
}

fn open_pinned(path: &Path) -> io::Result<File> {
    // READ_DATA (LIST_DIRECTORY for directories) participates in Windows share
    // checks. A handle opened for attributes alone would not prevent renames.
    OpenOptions::new()
        .access_mode(FILE_READ_DATA | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

pub(super) fn open_metadata(path: &Path) -> io::Result<ObservedMetadata> {
    let file = open_pinned(path)?;
    let metadata = file.metadata()?;
    let handle = file.as_raw_handle();
    let mut id = FILE_ID_INFO::default();
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // Both output structures have the Win32 layout and live across the calls.
    unsafe {
        if GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        ) == 0
            || GetFileInformationByHandle(handle, &mut info) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let bytes = id.FileId.Identifier;
    let inode = u64::from_le_bytes(bytes[..8].try_into().expect("eight-byte NTFS index"));
    if bytes[8..] != [0; 8] || inode == 0 || info.nNumberOfLinks == 0 {
        return Err(io::Error::other("unsupported Windows file identity"));
    }
    Ok(ObservedMetadata {
        metadata,
        pin: Arc::new(file),
        id: FileIdentity {
            device: id.VolumeSerialNumber,
            inode,
        },
        links: info.nNumberOfLinks,
        ancestors: Vec::new(),
    })
}

pub(super) fn resolve_inspection_root(path: &Path) -> io::Result<(PathBuf, ObservedMetadata)> {
    let absolute = std::path::absolute(path)?;
    if !matches!(absolute.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || absolute
            .components()
            .any(|part| part == Component::ParentDir)
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a local Windows drive is required",
        ));
    }
    let mut parents = absolute
        .ancestors()
        .skip(1)
        .take(MAX_DIRECTORY_DEPTH + 1)
        .collect::<Vec<_>>();
    if parents.len() > MAX_DIRECTORY_DEPTH {
        return Err(io::Error::other(
            "Windows root exceeds the directory handle budget",
        ));
    }
    let mut ancestors = Vec::with_capacity(parents.len());
    while let Some(parent) = parents.pop() {
        let pin = open_pinned(parent)?;
        require_directory(&pin.metadata()?)?;
        ancestors.push(pin);
    }

    let mut root = open_metadata(&absolute)?;
    require_directory(&root)?;
    let mut filesystem = [0u16; 32];
    // Only the filesystem type is requested. All optional volume outputs are null.
    if unsafe {
        GetVolumeInformationByHandleW(
            root.pin.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if !filesystem
        .iter()
        .copied()
        .take_while(|&ch| ch != 0)
        .eq("NTFS".encode_utf16())
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "NTFS is required for Windows inspection",
        ));
    }
    root.ancestors = ancestors;
    Ok((fs::canonicalize(&absolute)?, root))
}

fn require_directory(metadata: &Metadata) -> io::Result<()> {
    if !metadata.is_dir() || is_link_or_reparse_point(metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "root contains a reparse point or non-directory",
        ));
    }
    Ok(())
}
