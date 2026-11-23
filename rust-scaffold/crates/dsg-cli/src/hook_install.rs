use super::hooks::Agent;
use std::{io, path::Path};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn configure(
    agent: Agent,
    root: &Path,
    executable: &Path,
    install: bool,
) -> io::Result<bool> {
    local::configure(agent, root, executable, install)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn configure(_: Agent, _: &Path, _: &Path, _: bool) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "hook configuration updates are unavailable on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod local {
    use super::*;
    use serde::de::{DeserializeOwned, MapAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize};
    use serde_json::{json, value::RawValue, Value};
    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString};
    use std::fmt;
    use std::fs::{self, File, Metadata, OpenOptions};
    use std::io::{BufWriter, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    const MAX_CONFIG_BYTES: usize = 1024 * 1024;
    const MAX_EXECUTABLE_BYTES: usize = 4096;
    const OPEN_DIRECTORY: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    pub(super) fn configure(
        agent: Agent,
        root: &Path,
        executable: &Path,
        install: bool,
    ) -> io::Result<bool> {
        let (directory, name, agent_name, event, matcher) = match agent {
            Agent::Claude => (c".claude", c"settings.json", "claude", "PreToolUse", "Bash"),
            Agent::Codex => (c".codex", c"hooks.json", "codex", "PreToolUse", "Bash"),
            Agent::Gemini => (
                c".gemini",
                c"settings.json",
                "gemini",
                "BeforeTool",
                "run_shell_command",
            ),
        };
        let command = hook_command(executable, agent_name)?;
        let mut handler = json!({"type": "command", "command": command});
        match agent_name {
            "claude" => {
                handler["statusMessage"] = json!("DSG policy check");
                handler["onFailure"] = json!("block");
            }
            "codex" => handler["statusMessage"] = json!("DSG policy check"),
            _ => handler["name"] = json!("dsg-policy-check"),
        }

        if !root.is_absolute() || root.parent().is_none() || fs::canonicalize(root)? != root {
            return Err(unsafe_path());
        }
        let project = OpenOptions::new()
            .read(true)
            .custom_flags(OPEN_DIRECTORY)
            .open(root)?;
        verify_directory(root, &project)?;
        let mount = mount_id(&project)?;
        let Some(settings) = settings_directory(&project, directory, install)? else {
            return Ok(false);
        };
        if mount_id(&settings)? != mount {
            return Err(unsafe_path());
        }
        // A nonblocking directory lock serializes the installers without leaving a lock file.
        check_status(unsafe { libc::flock(settings.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) })?;
        let settings_path = root.join(directory.to_str().map_err(|_| unsafe_path())?);
        verify_directory(root, &project)?;
        verify_directory(&settings_path, &settings)?;

        let Some((mut config, previous)) = load_settings(&settings, name, &mount, install)? else {
            return Ok(false);
        };
        if !update(&mut config, event, matcher, &handler, install)? {
            return Ok(false);
        }

        let mut stage = StagedFile::new(&settings)?;
        {
            let mut writer = BoundedWriter(BufWriter::new(&mut stage.file), MAX_CONFIG_BYTES);
            serde_json::to_writer_pretty(&mut writer, &config).map_err(|_| invalid_config())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        if let Some(previous) = &previous {
            stage
                .file
                .set_permissions(fs::Permissions::from_mode(previous.mode() & 0o777))?;
        }
        stage.file.sync_all()?;

        verify_directory(root, &project)?;
        verify_directory(&settings_path, &settings)?;
        stage.commit(name, previous.as_ref(), &mount)?;
        Ok(true)
    }

    fn hook_command(executable: &Path, agent: &str) -> io::Result<String> {
        let path = executable
            .to_str()
            .filter(|path| {
                executable.is_absolute()
                    && path.len() <= MAX_EXECUTABLE_BYTES
                    && !path.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid DSG executable path")
            })?;
        // Break consecutive dollar and brace/name characters in addition to shell quoting.
        // Some clients expand project placeholders before passing the command to the shell.
        let path = path.replace('\'', "'\\''").replace('$', "'\\$'");
        Ok(format!("'{path}' hook --agent {agent}"))
    }

    fn update(
        config: &mut JsonObject,
        event: &str,
        matcher: &str,
        handler: &Value,
        install: bool,
    ) -> io::Result<bool> {
        let mut hooks: JsonObject = config.member("hooks")?.unwrap_or_default();
        let groups: Vec<Box<RawValue>> = hooks.member(event)?.unwrap_or_default();
        let mut updated = Vec::with_capacity(groups.len());
        let mut removed = false;
        for raw_group in groups {
            let Some((mut group, mut handlers)) = matching_group(&raw_group, matcher)? else {
                updated.push(raw_group);
                continue;
            };
            if install {
                if handlers.iter().any(|item| same_handler(item, handler)) {
                    return Ok(false);
                }
                updated.push(raw_group);
                continue;
            }
            let original_len = handlers.len();
            handlers.retain(|item| !same_handler(item, handler));
            if handlers.len() == original_len {
                updated.push(raw_group);
                continue;
            }
            removed = true;
            if !handlers.is_empty() || group.0.len() > 2 {
                group.insert("hooks", &handlers)?;
                updated
                    .push(serde_json::value::to_raw_value(&group).map_err(|_| invalid_config())?);
            }
        }
        if install {
            let group = json!({"matcher": matcher, "hooks": [handler]});
            updated.push(serde_json::value::to_raw_value(&group).map_err(|_| invalid_config())?);
        }
        if !install && !removed {
            return Ok(false);
        }
        if updated.is_empty() {
            hooks.0.remove(event);
        } else {
            hooks.insert(event, &updated)?;
        }
        if hooks.0.is_empty() {
            config.0.remove("hooks");
        } else {
            config.insert("hooks", &hooks)?;
        }
        Ok(true)
    }

    type HookGroup = (JsonObject, Vec<Box<RawValue>>);

    fn matching_group(raw: &RawValue, matcher: &str) -> io::Result<Option<HookGroup>> {
        if !raw.get().starts_with('{') {
            return Ok(None);
        }
        let group: JsonObject = serde_json::from_str(raw.get()).map_err(|_| invalid_config())?;
        if group.member::<String>("matcher").ok().flatten().as_deref() != Some(matcher) {
            return Ok(None);
        }
        let handlers = group.member("hooks")?.unwrap_or_default();
        Ok(Some((group, handlers)))
    }

    fn same_handler(candidate: &RawValue, expected: &Value) -> bool {
        // Compare every field. In particular, an extra or duplicate key makes a user entry distinct.
        let Ok(candidate) = serde_json::from_str::<JsonObject>(candidate.get()) else {
            return false;
        };
        let Some(expected) = expected.as_object() else {
            return false;
        };
        candidate.0.len() == expected.len()
            && expected.iter().all(|(key, value)| {
                candidate
                    .member::<Value>(key)
                    .is_ok_and(|item| item.as_ref() == Some(value))
            })
    }

    #[derive(Default, Serialize)]
    #[serde(transparent)]
    struct JsonObject(BTreeMap<String, Box<RawValue>>);

    impl JsonObject {
        fn member<T: DeserializeOwned>(&self, name: &str) -> io::Result<Option<T>> {
            self.0
                .get(name)
                .map(|value| serde_json::from_str(value.get()).map_err(|_| invalid_config()))
                .transpose()
        }

        fn insert(&mut self, name: &str, value: &impl Serialize) -> io::Result<()> {
            let value = serde_json::value::to_raw_value(value).map_err(|_| invalid_config())?;
            self.0.insert(name.to_owned(), value);
            Ok(())
        }
    }

    impl<'de> Deserialize<'de> for JsonObject {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct ObjectVisitor;

            impl<'de> Visitor<'de> for ObjectVisitor {
                type Value = JsonObject;

                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("a settings object with unique keys")
                }

                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                    let mut result = BTreeMap::new();
                    while let Some((key, value)) = map.next_entry()? {
                        if result.insert(key, value).is_some() {
                            return Err(serde::de::Error::custom("duplicate settings key"));
                        }
                    }
                    Ok(JsonObject(result))
                }
            }

            deserializer.deserialize_map(ObjectVisitor)
        }
    }

    fn settings_directory(parent: &File, name: &CStr, create: bool) -> io::Result<Option<File>> {
        match open_at(parent, name, OPEN_DIRECTORY) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                // `name` is a fixed, single component relative to the open project.
                let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
                if let Err(error) = check_status(result) {
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
                open_at(parent, name, OPEN_DIRECTORY).map(Some)
            }
            Err(error) => Err(error),
        }
    }

    fn load_settings(
        directory: &File,
        name: &CStr,
        mount: &MountId,
        install: bool,
    ) -> io::Result<Option<(JsonObject, Option<Metadata>)>> {
        let file = match open_settings(directory, name)? {
            Some(file) => file,
            None if install => return Ok(Some((JsonObject::default(), None))),
            None => return Ok(None),
        };
        let metadata = settings_metadata(&file, mount)?;
        if metadata.len() > MAX_CONFIG_BYTES as u64 {
            return Err(invalid_config());
        }
        let mut bytes = Vec::new();
        (&file)
            .take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(invalid_config());
        }
        if !same_version(&metadata, &file.metadata()?) {
            return Err(changed_config());
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| invalid_config())?;
        Ok(Some((value, Some(metadata))))
    }

    fn open_settings(directory: &File, name: &CStr) -> io::Result<Option<File>> {
        match open_at(
            directory,
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        ) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn settings_metadata(file: &File, mount: &MountId) -> io::Result<Metadata> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 || &mount_id(file)? != mount {
            return Err(unsafe_path());
        }
        Ok(metadata)
    }

    fn open_at(directory: &File, name: &CStr, flags: libc::c_int) -> io::Result<File> {
        // The directory and single-component C string outlive the system call.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // A successful openat returns a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn verify_directory(path: &Path, directory: &File) -> io::Result<()> {
        let current = fs::symlink_metadata(path)?;
        if !current.is_dir()
            || !same_identity(&current, &directory.metadata()?)
            || fs::canonicalize(path)? != path
        {
            return Err(unsafe_path());
        }
        Ok(())
    }

    fn same_identity(left: &Metadata, right: &Metadata) -> bool {
        left.dev() == right.dev() && left.ino() == right.ino()
    }

    fn same_version(left: &Metadata, right: &Metadata) -> bool {
        same_identity(left, right)
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }

    #[cfg(target_os = "linux")]
    type MountId = u64;
    #[cfg(target_os = "linux")]
    fn mount_id(file: &File) -> io::Result<MountId> {
        let mut info = std::mem::MaybeUninit::<libc::statx>::zeroed();
        // AT_EMPTY_PATH uses the open descriptor rather than resolving any path.
        check_status(unsafe {
            libc::statx(
                file.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                libc::STATX_MNT_ID,
                info.as_mut_ptr(),
            )
        })?;
        let info = unsafe { info.assume_init() };
        if info.stx_mask & libc::STATX_MNT_ID == 0 {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(info.stx_mnt_id)
    }

    #[cfg(target_os = "macos")]
    type MountId = u64;
    #[cfg(target_os = "macos")]
    fn mount_id(file: &File) -> io::Result<MountId> {
        let mut info = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        check_status(unsafe { libc::fstatfs(file.as_raw_fd(), info.as_mut_ptr()) })?;
        let info = unsafe { info.assume_init() };
        if info.f_flags & libc::MNT_LOCAL as u32 == 0 {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(file.metadata()?.dev())
    }

    fn check_status(status: libc::c_int) -> io::Result<()> {
        if status < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    struct StagedFile<'a> {
        directory: &'a File,
        name: CString,
        file: File,
    }

    impl<'a> StagedFile<'a> {
        fn new(directory: &'a File) -> io::Result<Self> {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce)
                .map_err(|_| io::Error::other("could not create a hook settings staging name"))?;
            let name = CString::new(format!(".dsg-hook-{:032x}.tmp", u128::from_ne_bytes(nonce)))
                .map_err(|_| unsafe_path())?;
            let file = open_at(
                directory,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )?;
            Ok(Self {
                directory,
                name,
                file,
            })
        }

        fn commit(
            &self,
            name: &CStr,
            previous: Option<&Metadata>,
            mount: &MountId,
        ) -> io::Result<()> {
            let dir = self.directory.as_raw_fd();
            let staged = open_settings(self.directory, &self.name)?.ok_or_else(changed_config)?;
            if !same_identity(&self.file.metadata()?, &settings_metadata(&staged, mount)?) {
                return Err(changed_config());
            }
            if let Some(previous) = previous {
                let current = open_settings(self.directory, name)?.ok_or_else(changed_config)?;
                if !same_version(previous, &settings_metadata(&current, mount)?) {
                    return Err(changed_config());
                }
                // Rename replaces the directory entry and never dereferences its target.
                check_status(unsafe { libc::renameat(dir, self.name.as_ptr(), dir, name.as_ptr()) })
            } else {
                // A new installation must not overwrite a file that appeared during the update.
                check_status(unsafe {
                    libc::linkat(dir, self.name.as_ptr(), dir, name.as_ptr(), 0)
                })
            }
        }
    }

    impl Drop for StagedFile<'_> {
        fn drop(&mut self) {
            // Only remove the temporary entry if it still points to the staging file.
            if let (Ok(Some(current)), Ok(staged)) = (
                open_settings(self.directory, &self.name),
                self.file.metadata(),
            ) {
                if current
                    .metadata()
                    .is_ok_and(|meta| same_identity(&meta, &staged))
                {
                    unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
                }
            }
        }
    }

    struct BoundedWriter<W>(W, usize);

    impl<W: Write> Write for BoundedWriter<W> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.1 {
                return Err(invalid_config());
            }
            let written = self.0.write(bytes)?;
            self.1 -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    fn unsafe_path() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "hook path is outside the supported project boundary",
        )
    }

    fn invalid_config() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid or oversized hook settings",
        )
    }

    fn changed_config() -> io::Error {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "hook settings changed during the update",
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::symlink;

        const EXECUTABLE: &str = "/opt/DSG package's/dsg";

        #[test]
        fn protects_executable_paths_from_client_placeholder_expansion() {
            let path = Path::new("/opt/${CLAUDE_PROJECT_DIR}/$GEMINI_PROJECT_DIR/dsg");
            assert_eq!(
                hook_command(path, "claude").unwrap(),
                "'/opt/'\\$'{CLAUDE_PROJECT_DIR}/'\\$'GEMINI_PROJECT_DIR/dsg' hook --agent claude"
            );
        }

        #[test]
        fn installs_and_removes_each_agent_schema_idempotently() {
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path().canonicalize().unwrap();
            let executable = Path::new(EXECUTABLE);
            for (agent, path, event, matcher, name) in [
                (
                    Agent::Claude,
                    ".claude/settings.json",
                    "PreToolUse",
                    "Bash",
                    "claude",
                ),
                (
                    Agent::Codex,
                    ".codex/hooks.json",
                    "PreToolUse",
                    "Bash",
                    "codex",
                ),
                (
                    Agent::Gemini,
                    ".gemini/settings.json",
                    "BeforeTool",
                    "run_shell_command",
                    "gemini",
                ),
            ] {
                assert!(!configure(agent, &root, executable, false).unwrap());
                let settings = root.join(path);
                assert!(!settings.parent().unwrap().exists());
                assert!(configure(agent, &root, executable, true).unwrap());
                let installed = fs::read(&settings).unwrap();
                let value: Value = serde_json::from_slice(&installed).unwrap();
                let group = &value["hooks"][event][0];
                let handler = &group["hooks"][0];
                assert_eq!(group["matcher"], matcher);
                assert_eq!(handler["type"], "command");
                assert_eq!(
                    handler["command"],
                    format!("'/opt/DSG package'\\''s/dsg' hook --agent {name}")
                );
                if name == "claude" {
                    assert_eq!(handler["onFailure"], "block");
                } else {
                    assert!(handler.get("onFailure").is_none());
                }
                assert!(!configure(agent, &root, executable, true).unwrap());
                assert_eq!(fs::read(&settings).unwrap(), installed);
                assert!(configure(agent, &root, executable, false).unwrap());
                assert!(!configure(agent, &root, executable, false).unwrap());
                let remaining: Value =
                    serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
                assert_eq!(remaining, json!({}));
            }
        }

        #[test]
        fn removes_only_the_exact_handler_and_preserves_raw_values() {
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path().canonicalize().unwrap();
            let executable = Path::new(EXECUTABLE);
            let directory = root.join(".claude");
            fs::create_dir(&directory).unwrap();
            let settings = directory.join("settings.json");
            let exact_number = "1234567890123456789012345678901234567890";
            let synthetic = format!(
                r#"{{"example": {{"large": {exact_number}}}, "hooks": {{"PostToolUse": []}}}}"#
            );
            fs::write(&settings, &synthetic).unwrap();
            assert!(configure(Agent::Claude, &root, executable, true).unwrap());
            assert!(fs::read_to_string(&settings)
                .unwrap()
                .contains(exact_number));
            let mut installed: Value =
                serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
            let group = &mut installed["hooks"]["PreToolUse"][0];
            group["sequential"] = json!(true);
            let handlers = group["hooks"].as_array_mut().unwrap();
            let mut user_handler = handlers[0].clone();
            user_handler["timeout"] = json!(10);
            handlers.push(user_handler.clone());
            let unrelated = json!({"type": "command", "command": "/opt/unrelated"});
            handlers.push(unrelated.clone());
            fs::write(&settings, serde_json::to_vec(&installed).unwrap()).unwrap();
            assert!(configure(Agent::Claude, &root, executable, false).unwrap());
            let remaining: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
            assert_eq!(remaining["hooks"]["PostToolUse"], json!([]));
            assert_eq!(
                remaining["hooks"]["PreToolUse"][0]["hooks"],
                json!([user_handler, unrelated])
            );
            assert_eq!(remaining["hooks"]["PreToolUse"][0]["sequential"], true);
        }

        #[test]
        fn rejects_config_aliases_and_leaves_external_files_unchanged() {
            let fixture = tempfile::tempdir().unwrap();
            let external = tempfile::tempdir().unwrap();
            let root = fixture.path().canonicalize().unwrap();
            let executable = Path::new(EXECUTABLE);
            let codex = root.join(".codex");
            let claude = root.join(".claude");
            let gemini = root.join(".gemini");
            let outside_file = external.path().join("synthetic.json");
            fs::write(&outside_file, b"{}\n").unwrap();
            symlink(external.path(), codex).unwrap();
            fs::create_dir(&claude).unwrap();
            fs::create_dir(&gemini).unwrap();
            symlink(&outside_file, claude.join("settings.json")).unwrap();
            fs::hard_link(&outside_file, gemini.join("settings.json")).unwrap();
            for agent in [Agent::Codex, Agent::Claude, Agent::Gemini] {
                assert!(configure(agent, &root, executable, true).is_err());
                assert!(configure(agent, &root, executable, false).is_err());
            }
            assert_eq!(fs::read(&outside_file).unwrap(), b"{}\n");
            assert!(!external.path().join("hooks.json").exists());
        }

        #[test]
        fn rejects_ambiguous_and_oversized_config_without_replacement() {
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path().canonicalize().unwrap();
            let executable = Path::new(EXECUTABLE);
            let directory = root.join(".codex");
            fs::create_dir(&directory).unwrap();
            let settings = directory.join("hooks.json");
            for bytes in [
                br#"{"hooks": {}, "hooks": {}}"#.to_vec(),
                br#"{"hooks": {"PreToolUse": false}}"#.to_vec(),
                vec![b' '; MAX_CONFIG_BYTES + 1],
            ] {
                fs::write(&settings, &bytes).unwrap();
                assert_eq!(
                    configure(Agent::Codex, &root, executable, true)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(fs::read(&settings).unwrap(), bytes);
            }
            assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
        }
    }
}
