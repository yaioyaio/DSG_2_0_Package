//go:build linux

package cache

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"hash"
	"io"
	"os"
	"slices"
	"strconv"
	"strings"

	"github.com/containerd/containerd/v2/core/snapshots"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

const (
	dsgAllocationMaxEntries = 4096
	dsgAllocationMaxDepth   = 32
	dsgAllocationStatMask   = unix.STATX_TYPE | unix.STATX_MODE | unix.STATX_NLINK | unix.STATX_UID |
		unix.STATX_GID | unix.STATX_MTIME | unix.STATX_CTIME | unix.STATX_INO |
		unix.STATX_SIZE | unix.STATX_BLOCKS | unix.STATX_MNT_ID
)

type DSGGraphAllocations struct {
	Snapshot *DSGGraphAllocation     `json:"snapshot,omitempty"`
	Blob     *DSGGraphBlobAllocation `json:"blob,omitempty"`
}

// DSGGraphAllocation records one complete, bounded walk of filesystem
// metadata. Allocated blocks are the stat total, not a reclaimable space
// measurement. File payloads and extended attributes remain unexamined.
type DSGGraphAllocation struct {
	MetadataSHA256     string `json:"metadata_sha256"`
	RootInode          uint64 `json:"root_inode"`
	Entries            int    `json:"entries"`
	AllocatedBlocks512 uint64 `json:"allocated_blocks_512"`
	ContentVerified    bool   `json:"content_verified"`
}

type DSGGraphBlobAllocation struct {
	DSGGraphAllocation
	DeclaredSHA256 string `json:"declared_sha256"`
}

type dsgAllocationStat struct {
	MountID, Inode, Size, Blocks uint64
	DeviceMajor, DeviceMinor     uint32
	UID, GID, Links              uint32
	Mode                         uint16
	MTimeSec, CTimeSec           int64
	MTimeNsec, CTimeNsec         uint32
}

func dsgAllocationStatAt(fd int, name string) (dsgAllocationStat, error) {
	var raw unix.Statx_t
	flags := unix.AT_SYMLINK_NOFOLLOW | unix.AT_NO_AUTOMOUNT
	if name == "" {
		flags |= unix.AT_EMPTY_PATH
	}
	if err := unix.Statx(fd, name, flags, dsgAllocationStatMask, &raw); err != nil ||
		raw.Mask&dsgAllocationStatMask != dsgAllocationStatMask || raw.Mnt_id == 0 || raw.Ino == 0 {
		return dsgAllocationStat{}, ErrDSGExactPreserve
	}
	return dsgAllocationStat{
		MountID: raw.Mnt_id, Inode: raw.Ino, Size: raw.Size, Blocks: raw.Blocks,
		DeviceMajor: raw.Dev_major, DeviceMinor: raw.Dev_minor,
		UID: raw.Uid, GID: raw.Gid, Links: raw.Nlink, Mode: raw.Mode,
		MTimeSec: raw.Mtime.Sec, CTimeSec: raw.Ctime.Sec,
		MTimeNsec: raw.Mtime.Nsec, CTimeNsec: raw.Ctime.Nsec,
	}, nil
}

func (s dsgAllocationStat) ownedBy(anchor dsgAllocationStat) bool {
	return s.MountID == anchor.MountID && s.DeviceMajor == anchor.DeviceMajor &&
		s.DeviceMinor == anchor.DeviceMinor && s.UID == anchor.UID && s.Mode&0022 == 0
}

func dsgAllocationChild(fd int, name string, anchor dsgAllocationStat) (dsgAllocationStat, error) {
	if len(name) == 0 || len(name) > 255 || name == "." || name == ".." || strings.ContainsAny(name, "/\x00") {
		return dsgAllocationStat{}, ErrDSGExactPreserve
	}
	st, err := dsgAllocationStatAt(fd, name)
	if err != nil || !st.ownedBy(anchor) {
		return dsgAllocationStat{}, ErrDSGExactPreserve
	}
	return st, nil
}

func dsgAllocationOpen(parent int, name string, want dsgAllocationStat, flags uint64) (*os.File, error) {
	fd, err := unix.Openat2(parent, name, &unix.OpenHow{
		Flags:   flags | unix.O_RDONLY | unix.O_CLOEXEC | unix.O_NONBLOCK | unix.O_NOATIME,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS,
	})
	if err != nil {
		return nil, ErrDSGExactPreserve
	}
	if got, err := dsgAllocationStatAt(fd, ""); err != nil || got != want {
		unix.Close(fd)
		return nil, ErrDSGExactPreserve
	}
	return os.NewFile(uintptr(fd), "dsg-allocation"), nil
}

type dsgAllocationWalk struct {
	ctx    context.Context
	anchor dsgAllocationStat
	digest hash.Hash
	out    DSGGraphAllocation
}

func dsgAllocationMatches(parent int, name string, want dsgAllocationStat) bool {
	got, err := dsgAllocationStatAt(parent, name)
	return err == nil && got == want
}

func (w *dsgAllocationWalk) entry(parent int, name string, st dsgAllocationStat, depth int) error {
	if w.ctx.Err() != nil || w.out.Entries >= dsgAllocationMaxEntries || depth >= dsgAllocationMaxDepth ||
		st.Blocks > ^uint64(0)-w.out.AllocatedBlocks512 {
		return ErrDSGExactPreserve
	}
	kind := st.Mode & unix.S_IFMT
	if (kind != unix.S_IFDIR && kind != unix.S_IFREG) || (kind == unix.S_IFREG && st.Links != 1) {
		return ErrDSGExactPreserve
	}
	// Length framing preserves arbitrary Linux filename bytes and keeps them
	// out of the public graph. Boundary markers distinguish directory levels.
	w.digest.Write(binary.BigEndian.AppendUint16([]byte{1}, uint16(len(name))))
	w.digest.Write([]byte(name))
	if err := binary.Write(w.digest, binary.BigEndian, st); err != nil {
		return ErrDSGExactPreserve
	}
	w.out.Entries++
	w.out.AllocatedBlocks512 += st.Blocks
	if kind == unix.S_IFDIR {
		dir, err := dsgAllocationOpen(parent, name, st, unix.O_DIRECTORY)
		if err != nil {
			return err
		}
		defer dir.Close()
		fd := int(dir.Fd())
		remaining := dsgAllocationMaxEntries - w.out.Entries
		names, err := dir.Readdirnames(remaining + 1)
		if w.ctx.Err() != nil || len(names) > remaining || (err != nil && !errors.Is(err, io.EOF)) {
			return ErrDSGExactPreserve
		}
		slices.Sort(names)
		for i, child := range names {
			if i > 0 && names[i-1] == child {
				return ErrDSGExactPreserve
			}
			info, err := dsgAllocationChild(fd, child, w.anchor)
			if err != nil || w.entry(fd, child, info, depth+1) != nil {
				return ErrDSGExactPreserve
			}
		}
	}
	if !dsgAllocationMatches(parent, name, st) {
		return ErrDSGExactPreserve
	}
	w.digest.Write([]byte{0})
	return nil
}

func dsgBindAllocationAncestor(h hash.Hash, name string, st dsgAllocationStat) error {
	// Bind the directory identity without including times or block counts
	// that could change when a sibling allocation is modified.
	h.Write(binary.BigEndian.AppendUint16([]byte{2}, uint16(len(name))))
	h.Write([]byte(name))
	return binary.Write(h, binary.BigEndian, []uint64{
		st.MountID, st.Inode, uint64(st.DeviceMajor), uint64(st.DeviceMinor), uint64(st.UID), uint64(st.Mode),
	})
}

// dsgObserveAllocation traverses only the selected allocation. The opened
// ancestor directories stay pinned until the walk and its final checks end.
func dsgObserveAllocation(ctx context.Context, root int, parts []string, ownSnapshotter *dsgAllocationStat, directory bool, size uint64) (DSGGraphAllocation, error) {
	var empty DSGGraphAllocation
	anchor, err := dsgAllocationStatAt(root, "")
	if err != nil || anchor.UID != uint32(os.Geteuid()) || anchor.Mode&unix.S_IFMT != unix.S_IFDIR ||
		anchor.Mode&0022 != 0 || len(parts) == 0 || len(parts) > dsgAllocationMaxDepth {
		return empty, ErrDSGExactPreserve
	}
	w := dsgAllocationWalk{ctx: ctx, anchor: anchor, digest: sha256.New()}
	w.digest.Write([]byte("dsg-native-allocation-metadata-v1\x00"))
	if dsgBindAllocationAncestor(w.digest, "", anchor) != nil {
		return empty, ErrDSGExactPreserve
	}
	fd := root
	type binding struct {
		parent int
		name   string
		stat   dsgAllocationStat
	}
	bindings := make([]binding, 0, len(parts)-1)
	for i, part := range parts[:len(parts)-1] {
		st, err := dsgAllocationChild(fd, part, anchor)
		if err != nil || st.Mode&unix.S_IFMT != unix.S_IFDIR || (i == 0 && ownSnapshotter != nil && st != *ownSnapshotter) {
			return empty, ErrDSGExactPreserve
		}
		if dsgBindAllocationAncestor(w.digest, part, st) != nil {
			return empty, ErrDSGExactPreserve
		}
		dir, err := dsgAllocationOpen(fd, part, st, unix.O_DIRECTORY)
		if err != nil {
			return empty, err
		}
		defer dir.Close()
		bindings = append(bindings, binding{parent: fd, name: part, stat: st})
		fd = int(dir.Fd())
	}
	name := parts[len(parts)-1]
	st, err := dsgAllocationChild(fd, name, anchor)
	want := uint16(unix.S_IFREG)
	if directory {
		want = unix.S_IFDIR
	}
	if err != nil || st.Mode&unix.S_IFMT != want || (!directory && st.Size != size) || w.entry(fd, name, st, 0) != nil {
		return empty, ErrDSGExactPreserve
	}
	for i := len(bindings) - 1; i >= 0; i-- {
		b := bindings[i]
		if !dsgAllocationMatches(b.parent, b.name, b.stat) {
			return empty, ErrDSGExactPreserve
		}
	}
	if ctx.Err() != nil || !dsgAllocationMatches(root, "", anchor) {
		return empty, ErrDSGExactPreserve
	}
	w.out.MetadataSHA256 = hex.EncodeToString(w.digest.Sum(nil))
	w.out.RootInode = st.Inode
	return w.out, nil
}

func (r *dsgGraphReader) allocations(o *DSGDedicatedGraph, tx *bolt.Tx, workerFD, snapshotterFD int, native dsgGraphNativeKeys, blob string, size *int64, warn func(string)) {
	out := &DSGGraphAllocations{}
	if o.Snapshot != nil {
		observed, err := r.snapshotAllocation(tx, workerFD, snapshotterFD, native)
		if err != nil {
			warn("native_snapshot_allocation_ownership")
		} else {
			out.Snapshot = &observed
		}
	}
	if o.Blob != nil {
		declared, ok := strings.CutPrefix(blob, "sha256:")
		if !ok || !dsgDigest.MatchString(declared) || size == nil || *size < 0 {
			warn("native_blob_allocation_ownership")
		} else if observed, err := dsgObserveAllocation(r.ctx, workerFD,
			[]string{"content", "blobs", "sha256", declared}, nil, false, uint64(*size)); err != nil {
			warn("native_blob_allocation_ownership")
		} else {
			out.Blob = &DSGGraphBlobAllocation{DSGGraphAllocation: observed, DeclaredSHA256: declared}
		}
	}
	if out.Snapshot != nil || out.Blob != nil {
		o.Allocations = out
	}
}

func (r *dsgGraphReader) snapshotAllocation(tx *bolt.Tx, workerFD, snapshotterFD int, native dsgGraphNativeKeys) (DSGGraphAllocation, error) {
	if tx == nil || tx.Writable() {
		return DSGGraphAllocation{}, ErrDSGExactPreserve
	}
	records, err := dsgGraphBucket(tx.Bucket([]byte("v1")), "snapshots")
	if err != nil || records == nil {
		return DSGGraphAllocation{}, ErrDSGExactPreserve
	}
	target, err := dsgGraphReadNativeSnapshot(records.Bucket([]byte(native.key)))
	if err != nil || target.kind != snapshots.KindCommitted || target.parent != native.parent {
		return DSGGraphAllocation{}, ErrDSGExactPreserve
	}
	snapshotter, err := dsgAllocationStatAt(snapshotterFD, "")
	if err != nil {
		return DSGGraphAllocation{}, err
	}
	return dsgObserveAllocation(r.ctx, workerFD,
		[]string{"snapshots", "snapshots", strconv.FormatUint(target.id, 10)}, &snapshotter, true, 0)
}
