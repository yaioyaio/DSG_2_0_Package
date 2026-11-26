//go:build linux && arm64

package dsgimage

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"syscall"

	"golang.org/x/sys/unix"
)

const (
	allocationScope      = "dedicated_ext4"
	allocationBlockBytes = 4096
	unstampedDigest      = "0000000000000000000000000000000000000000000000000000000000000000"
)

type allocation struct {
	Scope      string `json:"scope"`
	BlockBytes uint64 `json:"block_bytes"`
	Estimated  uint64 `json:"estimated_exclusive_layer_bytes"`
	Before     uint64 `json:"ext4_before_bytes"`
	After      uint64 `json:"ext4_after_bytes"`
	Net        int64  `json:"ext4_net_reclaimed_bytes"`
}

type allocationSample struct {
	fsid         unix.Fsid
	blocks, used uint64
}

func sampleAllocation(dir *os.File) (allocationSample, bool) {
	fd := int(dir.Fd())
	var stat unix.Statfs_t
	if unix.Syncfs(fd) != nil || unix.Fstatfs(fd, &stat) != nil ||
		stat.Type != unix.EXT4_SUPER_MAGIC || stat.Frsize != allocationBlockBytes ||
		stat.Blocks == 0 || stat.Blocks > math.MaxInt64/allocationBlockBytes || stat.Bfree > stat.Blocks {
		return allocationSample{}, false
	}
	return allocationSample{fsid: stat.Fsid, blocks: stat.Blocks, used: (stat.Blocks - stat.Bfree) * allocationBlockBytes}, true
}

func (before allocationSample) compare(after allocationSample, estimate uint64) *allocation {
	if before.blocks != after.blocks || before.fsid != after.fsid || estimate > math.MaxInt64 {
		return nil
	}
	return &allocation{Scope: allocationScope, BlockBytes: allocationBlockBytes, Estimated: estimate,
		Before: before.used, After: after.used, Net: int64(before.used) - int64(after.used)}
}

func (a *allocation) valid() bool {
	return a.Scope == allocationScope && a.BlockBytes == allocationBlockBytes &&
		a.Estimated <= math.MaxInt64 && a.Before <= math.MaxInt64 && a.After <= math.MaxInt64 &&
		a.Before%allocationBlockBytes == 0 && a.After%allocationBlockBytes == 0 &&
		a.Net == int64(a.Before)-int64(a.After)
}

func (a *allocation) reclaimed() uint64 {
	if a == nil || a.Net <= 0 {
		return 0
	}
	return uint64(a.Net)
}

type sealedReceipt struct {
	Receipt receipt `json:"receipt"`
	SHA256  string  `json:"sha256"`
}

func sealDigest(value receipt) [32]byte {
	// The persisted structure contains only integers, bounded strings and an
	// optional allocation. Its canonical digest detects an interrupted write.
	payload, _ := json.Marshal(value)
	return sha256.Sum256(payload)
}

func (s *service) reserveReceipt(name string) (*os.File, error) {
	fd, err := unix.Openat(int(s.ledger.Fd()), name, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0600)
	if err != nil {
		return nil, errBoundary
	}
	f := os.NewFile(uintptr(fd), name)
	n, err := f.Write(make([]byte, allocationBlockBytes))
	if err != nil || n != allocationBlockBytes || f.Sync() != nil || s.ledger.Sync() != nil || !receiptSlotAllocated(f) {
		f.Close()
		return nil, errBoundary
	}
	return f, nil
}

func receiptSlotAllocated(f *os.File) bool {
	info, err := f.Stat()
	return err == nil && info.Size() == allocationBlockBytes &&
		info.Sys().(*syscall.Stat_t).Blocks == allocationBlockBytes/512
}

func stageReceipt(f *os.File, value receipt) (int64, error) {
	payload, err := json.Marshal(sealedReceipt{Receipt: value, SHA256: unstampedDigest})
	if err != nil || len(payload) >= allocationBlockBytes || !receiptSlotAllocated(f) {
		return 0, errBoundary
	}
	offset := bytes.LastIndex(payload, []byte(unstampedDigest))
	if offset < 0 {
		return 0, errBoundary
	}
	slot := bytes.Repeat([]byte{' '}, allocationBlockBytes)
	copy(slot, payload)
	n, err := f.WriteAt(slot, 0)
	if err != nil || n != len(slot) || f.Sync() != nil || !receiptSlotAllocated(f) {
		return 0, errBoundary
	}
	return int64(offset), nil
}

func (s *service) settleReceipt(f *os.File, value receipt, before allocationSample, estimate uint64) error {
	after, ok := sampleAllocation(s.dir)
	if !ok || !s.current() {
		return errBoundary
	}
	value.Allocation = before.compare(after, estimate)
	value.Bytes = value.Allocation.reclaimed()
	offset, err := stageReceipt(f, value)
	if err != nil {
		return err
	}
	// All blocks, including the receipt's final name, were allocated before
	// sampling. The final checksum rewrites bytes within the same ext4 block.
	if check, held := sampleAllocation(s.dir); !held || !s.current() {
		return errBoundary
	} else if check != after {
		value.Allocation = nil
		value.Bytes = 0
		offset, err = stageReceipt(f, value)
		if err != nil {
			return err
		}
	}
	checksum := fmt.Sprintf("%x", sealDigest(value))
	n, err := f.WriteAt([]byte(checksum), offset)
	if err != nil || n != len(checksum) || f.Sync() != nil {
		return errBoundary
	}
	return s.ledger.Sync()
}

func decodeReceipt(data []byte) *receipt {
	if len(data) < allocationBlockBytes {
		var legacy receipt
		if decode(data, &legacy) == nil && legacy.Bytes == 0 && legacy.Allocation == nil {
			return &legacy
		}
		return nil
	}
	var value sealedReceipt
	if len(data) != allocationBlockBytes || decode(data, &value) != nil ||
		value.SHA256 == unstampedDigest || value.SHA256 != fmt.Sprintf("%x", sealDigest(value.Receipt)) {
		return nil
	}
	got := value.Receipt
	if (got.Allocation != nil && !got.Allocation.valid()) || got.Bytes != got.Allocation.reclaimed() {
		return nil
	}
	return &got
}
