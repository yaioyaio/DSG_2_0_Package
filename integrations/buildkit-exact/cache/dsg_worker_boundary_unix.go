//go:build linux || darwin

package cache

import (
	"os"
	"path/filepath"
	"runtime"

	"golang.org/x/sys/unix"
)

func dsgProtectedParent(root string) bool {
	parent := filepath.Dir(root)
	resolved, err := filepath.EvalSymlinks(parent)
	if err != nil {
		return false
	}
	// macOS exposes system-owned /var and /tmp aliases to its private tree.
	if resolved != parent && !(runtime.GOOS == "darwin" && resolved == "/private"+parent) {
		return false
	}
	for dir := resolved; ; dir = filepath.Dir(dir) {
		var st unix.Stat_t
		if unix.Lstat(dir, &st) != nil || st.Mode&unix.S_IFMT != unix.S_IFDIR ||
			(st.Uid != 0 && int(st.Uid) != os.Geteuid()) ||
			(st.Mode&0022 != 0 && st.Mode&unix.S_ISVTX == 0) {
			return false
		}
		if dir == "/" {
			return true
		}
	}
}

// No removable lock file is involved. An inherited directory can be reached by
// multiple paths, but independently opened descriptors contend on the same inode.
func dsgLockRoot(root string) (int, error) {
	if !filepath.IsAbs(root) || root != filepath.Clean(root) || root == "/" || !dsgProtectedParent(root) {
		return -1, ErrDSGExactPreserve
	}
	fd, err := unix.Open(root, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return -1, ErrDSGExactPreserve
	}
	var owned, named unix.Stat_t
	if unix.Fstat(fd, &owned) != nil || owned.Mode&07777 != 0700 || int(owned.Uid) != os.Geteuid() ||
		unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB) != nil || unix.Lstat(root, &named) != nil || owned.Dev != named.Dev || owned.Ino != named.Ino {
		unix.Close(fd)
		return -1, ErrDSGExactPreserve
	}
	return fd, nil
}

func dsgWithWorkerRoot(root string, run func() error) error {
	if run == nil {
		return ErrDSGExactPreserve
	}
	fd, err := dsgLockRoot(root)
	if err != nil {
		return err
	}
	defer unix.Close(fd)
	return run()
}
