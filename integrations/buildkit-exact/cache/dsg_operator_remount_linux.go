//go:build linux

package cache

import (
	"golang.org/x/sys/unix"
	"strconv"
	"strings"
)

// The kernel follows the pinned mountpoint descriptor while the enrolled
// child directory remains exclusively locked.
func dsgCustodyRemount(fd int, readOnly bool) error {
	parent, err := unix.Openat(fd, "..", unix.O_PATH|unix.O_NOFOLLOW|unix.O_DIRECTORY|unix.O_CLOEXEC, 0)
	if err != nil {
		return ErrDSGExactPreserve
	}
	defer unix.Close(parent)
	pst, err := dsgAllocationStatAt(parent, "")
	if err != nil {
		return err
	}
	child, err := dsgAllocationStatAt(fd, "")
	if err != nil || pst.MountID != child.MountID {
		return ErrDSGExactPreserve
	}
	mounts, err := dsgMounts("/proc/self/mountinfo")
	if err != nil {
		return err
	}
	for _, m := range mounts {
		if m.id != strconv.FormatUint(pst.MountID, 10) {
			continue
		}
		flags := uintptr(unix.MS_REMOUNT)
		if readOnly {
			flags |= unix.MS_RDONLY
		}
		for _, opt := range strings.Split(m.options, ",") {
			switch opt {
			case "rw", "ro":
			case "nodev":
				flags |= unix.MS_NODEV
			case "noexec":
				flags |= unix.MS_NOEXEC
			case "nosuid":
				flags |= unix.MS_NOSUID
			case "nosymfollow":
				flags |= unix.MS_NOSYMFOLLOW
			case "relatime":
				flags |= unix.MS_RELATIME
			case "strictatime":
				flags |= unix.MS_STRICTATIME
			case "nodiratime":
				flags |= unix.MS_NODIRATIME
			case "noatime":
				flags |= unix.MS_NOATIME
			default:
				return ErrDSGExactPreserve
			}
		}
		return unix.Mount("", "/proc/self/fd/"+strconv.Itoa(parent), "", flags, "")
	}
	return ErrDSGExactPreserve
}
