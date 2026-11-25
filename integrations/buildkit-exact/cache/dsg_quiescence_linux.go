//go:build linux

package cache

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"golang.org/x/sys/unix"
)

// Strict enrollment is single-generation. The private service is PID 1 in
// its own PID namespace. A read-only ext4 superblock fences filesystem writes
// across mount namespaces, including between the two visibility snapshots.
// The controlling VM kernel, storage backend and administrators are trusted.
const (
	dsgNSMarker = ".dsg-pid-namespace-v1"
	// Linux reserves this nsfs inode for the initial PID namespace.
	dsgInitPIDNS uint64 = 0xeffffffc
)

type DSGDedicatedQuiescence struct {
	FilesystemReadOnly    bool `json:"filesystem_read_only"`
	ExecutorNamespaceGone bool `json:"executor_namespace_gone"`
	MountsClear           bool `json:"mounts_clear"`
	ExternalHandlesClear  bool `json:"external_handles_clear"`
}

func dsgQError(reason string) error {
	return fmt.Errorf("quiescence_%s: %w", reason, ErrDSGExactPreserve)
}

func dsgReadKernel(path string, limit int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, limit+1))
	if int64(len(data)) > limit {
		return nil, dsgQError("limits")
	}
	return data, err
}

func dsgStat(path string) (unix.Stat_t, error) {
	var st unix.Stat_t
	err := unix.Stat(path, &st)
	return st, err
}

func dsgEnrollNamespace(dir int, root string) error {
	ns, err := dsgStat("/proc/self/ns/pid")
	if err != nil || os.Geteuid() != 0 || os.Getpid() != 1 || ns.Ino == dsgInitPIDNS {
		return dsgQError("namespace")
	}
	procInit, err := dsgStat("/proc/1/ns/pid")
	if err != nil || ns.Dev != procInit.Dev || ns.Ino != procInit.Ino {
		return dsgQError("enrollment")
	}
	st, err := dsgFilesystem(dir, root, false)
	if err != nil {
		return err
	}
	data, err := dsgGenerationIdentity(dir, st, ns.Ino)
	if err != nil {
		return err
	}
	fd, err := unix.Openat(dir, dsgNSMarker, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0600)
	if err != nil {
		return dsgQError("enrollment")
	}
	defer unix.Close(fd)
	n, err := unix.Write(fd, []byte(data))
	if err != nil || n != len(data) || unix.Fsync(fd) != nil || unix.Fsync(dir) != nil {
		return dsgQError("enrollment")
	}
	return nil
}

// Bind a namespace generation to its enrolled filesystem and directory inode.
// A copied marker in another directory cannot qualify that directory.
func dsgGenerationIdentity(dir int, st unix.Stat_t, ns uint64) (string, error) {
	var fs unix.Statfs_t
	if unix.Fstatfs(dir, &fs) != nil {
		return "", dsgQError("filesystem")
	}
	return fmt.Sprintf("%d:%x:%x:%08x%08x", ns, st.Dev, st.Ino,
		uint32(fs.Fsid.Val[0]), uint32(fs.Fsid.Val[1])), nil
}

type dsgMount struct {
	id, parent, device, root, path, options, fs, super string
	private                                            bool
}

func dsgMounts(path string) ([]dsgMount, error) {
	data, err := dsgReadKernel(path, 1<<20)
	if err != nil {
		return nil, dsgQError("visibility_mount_table")
	}
	var result []dsgMount
	for _, line := range bytes.Split(bytes.TrimSuffix(data, []byte{'\n'}), []byte{'\n'}) {
		left, right, ok := strings.Cut(string(line), " - ")
		l, r := strings.Fields(left), strings.Fields(right)
		if !ok || len(l) < 6 || len(r) != 3 || len(result) >= 4096 {
			return nil, dsgQError("visibility_mount_entry")
		}
		result = append(result, dsgMount{l[0], l[1], l[2], l[3], l[4], l[5], r[0], r[2], len(l) == 6})
	}
	return result, nil
}

func dsgOption(list, word string) bool {
	for _, item := range strings.Split(list, ",") {
		if item == word {
			return true
		}
	}
	return false
}

func dsgFilesystem(dir int, root string, readOnly bool) (unix.Stat_t, error) {
	var st unix.Stat_t
	var fs unix.Statfs_t
	if unix.Fstat(dir, &st) != nil || unix.Fstatfs(dir, &fs) != nil || fs.Type != unix.EXT4_SUPER_MAGIC ||
		(fs.Flags&unix.ST_RDONLY != 0) != readOnly {
		return st, dsgQError("filesystem")
	}
	name, err := dsgStat(root)
	if err != nil || name.Dev != st.Dev || name.Ino != st.Ino {
		return st, dsgQError("filesystem")
	}
	mounts, err := dsgMounts("/proc/self/mountinfo")
	if err != nil {
		return st, err
	}
	device := fmt.Sprintf("%d:%d", unix.Major(uint64(st.Dev)), unix.Minor(uint64(st.Dev)))
	return st, dsgCheckMountTree(mounts, device, root, readOnly)
}

func dsgCheckMountTree(mounts []dsgMount, device, root string, readOnly bool) error {
	expectedMode := "rw"
	if readOnly {
		expectedMode = "ro"
	}
	var target *dsgMount
	for i := range mounts {
		m := &mounts[i]
		if m.device != device {
			continue
		}
		if target != nil {
			return dsgQError("mounts")
		}
		target = m
	}
	// Restrict enrollment to a direct child of the private filesystem's
	// mountpoint. Unsupported escaped mountpoint names preserve.
	if target == nil || target.root != "/" || target.fs != "ext4" || !target.private ||
		target.path == "/" || target.path != filepath.Dir(root) ||
		!dsgOption(target.super, expectedMode) || !dsgOption(target.options, expectedMode) {
		return dsgQError("filesystem")
	}
	for _, m := range mounts {
		if m.parent == target.id {
			return dsgQError("mounts")
		}
	}
	return nil
}

func dsgNumericEntries(path string, limit int) ([]string, error) {
	if limit <= 0 {
		return nil, dsgQError("limits")
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	names, err := f.Readdirnames(limit + 1)
	if err != nil && !errors.Is(err, io.EOF) {
		return nil, err
	}
	if len(names) > limit {
		return nil, dsgQError("limits")
	}
	var ids []string
	for _, name := range names {
		if _, err := strconv.ParseUint(name, 10, 32); err == nil {
			ids = append(ids, name)
		}
	}
	return ids, nil
}

type dsgProcScan struct {
	device       uint64
	namespace    uint64
	selfNS       uint64
	nsDevice     uint64
	pinnedMounts map[uint64]bool
	lockFD       int
	mounts       map[uint64]bool
	pids         map[uint64]bool
	descriptors  int
	threads      int
	deadline     time.Time
}

func (s *dsgProcScan) checkPID(path string) error {
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC, 0)
	if err != nil {
		return dsgQError("visibility_pid_open")
	}
	var chain []uint64
	for depth := 0; ; depth++ {
		var st unix.Stat_t
		if depth > 32 || unix.Fstat(fd, &st) != nil {
			unix.Close(fd)
			return dsgQError("visibility_pid_stat")
		}
		if st.Ino == s.namespace {
			unix.Close(fd)
			return dsgQError("namespace")
		}
		if st.Ino == dsgInitPIDNS || s.pids[st.Ino] {
			unix.Close(fd)
			for _, ns := range chain {
				s.pids[ns] = true
			}
			return nil
		}
		chain = append(chain, st.Ino)
		parent, err := unix.IoctlRetInt(fd, unix.NS_GET_PARENT)
		unix.Close(fd)
		if err != nil {
			return dsgQError("visibility_pid_parent")
		}
		fd = parent
	}
}

func (s *dsgProcScan) checkMountNamespace(task string) error {
	st, err := dsgStat(task + "/ns/mnt")
	if err != nil {
		return dsgQError("visibility_mount_namespace")
	}
	if s.mounts[st.Ino] {
		return nil
	}
	if len(s.mounts) >= 128 {
		return dsgQError("limits")
	}
	mounts, err := dsgMounts(task + "/mountinfo")
	if err != nil {
		return err
	}
	device := fmt.Sprintf("%d:%d", unix.Major(s.device), unix.Minor(s.device))
	for _, m := range mounts {
		if st.Ino != s.selfNS && m.device == device {
			return dsgQError("mounts")
		}
		// A pinned mount namespace may survive its final task. This gate
		// does not enter it or assume its mount table is empty.
		if m.fs == "nsfs" && strings.Contains(m.root, "mnt:[") {
			return dsgQError("visibility_pinned_mount")
		}
	}
	// A chrooted namespace representative can expose a partial mountinfo
	// view. Certify another namespace only through a task with host root.
	if st.Ino != s.selfNS {
		foreign, err := dsgStat(task + "/root")
		host, hostErr := dsgStat("/")
		if err != nil || hostErr != nil || foreign.Dev != host.Dev || foreign.Ino != host.Ino {
			return dsgQError("visibility_chroot")
		}
	}
	s.mounts[st.Ino] = true
	return nil
}

// Fields before the closing parenthesis may contain spaces. Only kernel flags
// and state are interpreted, and neither process names nor paths are emitted.
func dsgNoUserContext(path string) (bool, error) {
	data, err := dsgReadKernel(path+"/stat", 4096)
	if err != nil {
		return false, err
	}
	end := bytes.LastIndexByte(data, ')')
	if end < 0 {
		return false, dsgQError("visibility_task_stat")
	}
	fields := bytes.Fields(data[end+1:])
	if len(fields) < 7 {
		return false, dsgQError("visibility_task_state")
	}
	flags, err := strconv.ParseUint(string(fields[6]), 10, 64)
	if err != nil {
		return false, dsgQError("visibility_task_flags")
	}
	return flags&0x00200000 != 0 || string(fields[0]) == "Z" || string(fields[0]) == "X", nil
}

func (s *dsgProcScan) checkTask(task string, self bool) error {
	noContext, err := dsgNoUserContext(task)
	if err != nil {
		return dsgQError("visibility_task_context")
	}
	if noContext {
		return nil
	}
	if err := s.checkPID(task + "/ns/pid"); err != nil {
		return err
	}
	if err := s.checkMountNamespace(task); err != nil {
		return err
	}
	for _, name := range []string{"cwd", "root", "exe"} {
		st, err := dsgStat(task + "/" + name)
		if err != nil {
			return dsgQError("visibility_task_reference")
		}
		if uint64(st.Dev) == s.device {
			return dsgQError("handles")
		}
	}
	files, err := dsgNumericEntries(task+"/fd", 65536-s.descriptors)
	if err != nil {
		return dsgQError("visibility_fds")
	}
	s.descriptors += len(files)
	for _, name := range files {
		if self && name == strconv.Itoa(s.lockFD) {
			continue
		}
		path := task + "/fd/" + name
		st, err := dsgStat(path)
		// Reading the inspector's FD directory creates a transient FD.
		if errors.Is(err, os.ErrNotExist) {
			if _, linkErr := os.Lstat(path); errors.Is(linkErr, os.ErrNotExist) {
				continue
			}
		}
		if err != nil {
			return dsgQError("visibility_fd_stat")
		}
		if uint64(st.Dev) == s.nsDevice {
			// A namespace FD can outlive every task in that namespace. Read
			// only its nsfs type. No target file or handle contents are read.
			target, err := os.Readlink(path)
			if err != nil {
				return dsgQError("visibility_ns_fd")
			}
			if target == fmt.Sprintf("mnt:[%d]", st.Ino) {
				s.pinnedMounts[st.Ino] = true
			}
		}
		if uint64(st.Dev) == s.device || st.Mode&unix.S_IFMT == unix.S_IFBLK && uint64(st.Rdev) == s.device {
			return dsgQError("handles")
		}
	}
	return nil
}

func dsgProcVisible() error {
	var fs unix.Statfs_t
	ns, err := dsgStat("/proc/self/ns/pid")
	init, initErr := dsgStat("/proc/1/ns/pid")
	if err != nil || initErr != nil || ns.Ino != dsgInitPIDNS || init.Ino != ns.Ino || init.Dev != ns.Dev ||
		os.Geteuid() != 0 || unix.Statfs("/proc", &fs) != nil || fs.Type != unix.PROC_SUPER_MAGIC {
		return dsgQError("visibility_initial_pid")
	}
	mounts, err := dsgMounts("/proc/self/mountinfo")
	if err != nil {
		return err
	}
	for _, m := range mounts {
		if m.path == "/proc" && m.root == "/" && m.fs == "proc" {
			for _, op := range strings.Split(m.super, ",") {
				if strings.HasPrefix(op, "hidepid=") && op != "hidepid=0" && op != "hidepid=off" {
					return dsgQError("visibility_hidden_proc")
				}
			}
			return nil
		}
	}
	return dsgQError("visibility_proc_mount")
}

func dsgCheckQuiescence(dir int, root string) error {
	if err := dsgProcVisible(); err != nil {
		return err
	}
	st, err := dsgFilesystem(dir, root, true)
	if err != nil {
		return err
	}
	marker, err := dsgOpenOwned(dir, dsgNSMarker, false)
	if err != nil {
		return dsgQError("enrollment")
	}
	data, err := io.ReadAll(io.LimitReader(marker, 96))
	marker.Close()
	prefix, _, framed := strings.Cut(string(data), ":")
	ns, parseErr := strconv.ParseUint(prefix, 10, 64)
	expected, identityErr := dsgGenerationIdentity(dir, st, ns)
	if err != nil || !framed || parseErr != nil || identityErr != nil || ns == 0 || ns == dsgInitPIDNS || expected != string(data) {
		return dsgQError("enrollment")
	}
	mnt, err := dsgStat("/proc/self/ns/mnt")
	if err != nil {
		return dsgQError("visibility_own_mount")
	}
	s := dsgProcScan{device: uint64(st.Dev), namespace: ns, selfNS: mnt.Ino, nsDevice: uint64(mnt.Dev), pinnedMounts: map[uint64]bool{}, lockFD: dir,
		mounts: map[uint64]bool{}, pids: map[uint64]bool{}, deadline: time.Now().Add(4 * time.Second)}
	pids, err := dsgNumericEntries("/proc", 4096)
	if err != nil {
		return dsgQError("visibility_pids")
	}
	for _, pid := range pids {
		process := "/proc/" + pid
		threads, err := dsgNumericEntries(process+"/task", 8192-s.threads)
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				continue
			}
			return dsgQError("visibility_tasks")
		}
		s.threads += len(threads)
		for _, tid := range threads {
			if time.Now().After(s.deadline) {
				return dsgQError("limits")
			}
			task := process + "/task/" + tid
			if err := s.checkTask(task, pid == strconv.Itoa(os.Getpid())); err != nil {
				// A now-exited task cannot retain an FD or mount namespace.
				if _, gone := os.Stat(task); errors.Is(gone, os.ErrNotExist) {
					continue
				}
				return err
			}
		}
	}
	for inode := range s.pinnedMounts {
		if !s.mounts[inode] {
			return dsgQError("visibility_orphan_mount")
		}
	}
	return nil
}
