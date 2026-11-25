//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

// This package-private boundary has no stock worker or remote mutation entry point.
// A supervisor separately owns the isolated mount namespace, authority store and remount capability.
const dsgSameMountOperation = "native-exact-record-reclaim/same-mount/v1"
const dsgSameMountBucket = "dsg-same-mount-grants-v1"

type dsgSameMountSource struct {
	Project, Worker, RecordID, PayloadSHA256, LayerSHA256, Origin, PlanSHA256, PayloadGeneration string
}

func (s dsgSameMountSource) valid() bool {
	return dsgFullID.MatchString(s.Worker) && dsgFullID.MatchString(s.RecordID) &&
		dsgDigest.MatchString(s.Project) && dsgDigest.MatchString(s.PayloadSHA256) &&
		dsgDigest.MatchString(s.LayerSHA256) && dsgDigest.MatchString(s.Origin) &&
		dsgDigest.MatchString(s.PlanSHA256) && dsgDigest.MatchString(s.PayloadGeneration)
}

type dsgSameMountIdentity struct {
	BootSHA256, EnrollmentSHA256                       string
	MountNS, MountID, UniqueMountID, RootInode, Device uint64
	FSID                                               [2]int32
}

type dsgSameMountRecord struct {
	GraphSHA256, MetadataDigest string
	DatabasesSHA256             [5]string
}

var dsgSameMountDatabases = [5]string{"solver.db", "metadata_v2.db", "containerd.db", "snapshots/metadata.db", "history.db"}

func dsgSameMountDatabaseHashes(ctx context.Context, dir int) (out [5]string, err error) {
	anchor, err := dsgAllocationStatAt(dir, "")
	if err != nil {
		return out, err
	}
	for i, name := range dsgSameMountDatabases {
		if ctx.Err() != nil {
			return out, ErrDSGExactPreserve
		}
		fd, err := unix.Openat2(dir, name, &unix.OpenHow{Flags: unix.O_RDONLY | unix.O_NOATIME | unix.O_NOFOLLOW | unix.O_NONBLOCK | unix.O_CLOEXEC, Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS})
		if err != nil {
			return out, ErrDSGExactPreserve
		}
		f := os.NewFile(uintptr(fd), "native-database")
		before, err := dsgAllocationStatAt(fd, "")
		if err != nil || !before.ownedBy(anchor) || before.Mode != unix.S_IFREG|0600 || before.Links != 1 || before.Size == 0 || before.Size > 64<<20 {
			f.Close()
			return out, ErrDSGExactPreserve
		}
		h := sha256.New()
		n, err := io.Copy(h, io.LimitReader(f, int64(before.Size)+1))
		after, checkErr := dsgAllocationStatAt(fd, "")
		closeErr := f.Close()
		if err != nil || checkErr != nil || closeErr != nil || n != int64(before.Size) || after != before {
			return out, ErrDSGExactPreserve
		}
		out[i] = hex.EncodeToString(h.Sum(nil))
	}
	return out, nil
}

func (r dsgSameMountRecord) valid() bool {
	if !dsgDigest.MatchString(r.GraphSHA256) || !dsgDigest.MatchString(r.MetadataDigest) {
		return false
	}
	for _, digest := range r.DatabasesSHA256 {
		if !dsgDigest.MatchString(digest) {
			return false
		}
	}
	return true
}

type dsgSameMountEvidence struct {
	Source     dsgSameMountSource
	Identity   dsgSameMountIdentity
	Generation string
	Record     dsgSameMountRecord
}

func (i dsgSameMountIdentity) generation() string {
	b, _ := json.Marshal(i)
	return dsgRecoveryHash(append([]byte("dsg-same-mount-generation-v1\x00"), b...))
}

type dsgSameMountGrant struct {
	Operation     string
	CustodySHA256 string
	Evidence      dsgSameMountEvidence
	Request       DSGExactIntent
}

func (g dsgSameMountGrant) valid() bool {
	e, r := g.Evidence, g.Request
	i, s := e.Identity, e.Source
	return g.Operation == dsgSameMountOperation && dsgDigest.MatchString(g.CustodySHA256) && s.valid() && r.valid() &&
		i.MountNS != 0 && i.MountID != 0 && i.UniqueMountID != 0 && i.RootInode != 0 && i.Device != 0 &&
		dsgDigest.MatchString(i.BootSHA256) && dsgDigest.MatchString(i.EnrollmentSHA256) &&
		e.Generation == i.generation() && e.Record.valid() &&
		r.Project == s.Project && r.Worker == s.Worker && r.RecordID == s.RecordID && r.Origin == s.Origin &&
		r.Generation == e.Generation && r.MetadataDigest == e.Record.MetadataDigest &&
		r.ExpiresAtMS-r.ApprovedAtMS <= (30*time.Minute).Milliseconds()
}

func (g dsgSameMountGrant) active(ctx context.Context) bool {
	now := time.Now().UnixMilli()
	return ctx.Err() == nil && now >= g.Request.ApprovedAtMS && now < g.Request.ExpiresAtMS
}

func (g dsgSameMountGrant) matches(receipt DSGExactReceipt) bool {
	return receipt.Request == g.Request && receipt.LogicalRecords == 1 && receipt.PhysicalBytes == nil
}

type dsgSameMountParticipant interface {
	evidence(context.Context, int, dsgSameMountSource) (dsgSameMountRecord, error)
	apply(context.Context, int, dsgSameMountGrant) (DSGExactReceipt, error)
	receipt(context.Context, int, dsgSameMountGrant) (DSGExactReceipt, error)
}

func dsgPinSameMount(fd int, root string, readOnly bool) (dsgSameMountIdentity, error) {
	var i dsgSameMountIdentity
	if !dsgValidMarker(fd) {
		return i, ErrDSGExactPreserve
	}
	if err := dsgCheckQuiescenceMode(fd, root, readOnly); err != nil {
		return i, err
	}
	return dsgSameMountID(fd, root)
}

func dsgSameMountID(fd int, root string) (dsgSameMountIdentity, error) {
	var i dsgSameMountIdentity
	st, err := dsgAllocationStatAt(fd, "")
	var fs unix.Statfs_t
	var unique, named unix.Statx_t
	mask := unix.STATX_MNT_ID_UNIQUE | unix.STATX_INO
	if err != nil || unix.Fstatfs(fd, &fs) != nil ||
		unix.Statx(fd, "", unix.AT_EMPTY_PATH, mask, &unique) != nil || unique.Mask&uint32(mask) != uint32(mask) ||
		unix.Statx(unix.AT_FDCWD, root, unix.AT_SYMLINK_NOFOLLOW, mask, &named) != nil || named.Mask&uint32(mask) != uint32(mask) ||
		unique.Mnt_id == 0 || unique.Mnt_id != named.Mnt_id || unique.Ino != named.Ino || st.Inode != named.Ino {
		return i, ErrDSGExactPreserve
	}
	ns, err := dsgStat("/proc/self/ns/mnt")
	if err != nil {
		return i, ErrDSGExactPreserve
	}
	boot, err := dsgReadKernel("/proc/sys/kernel/random/boot_id", 64)
	if err != nil || len(strings.TrimSpace(string(boot))) != 36 {
		return i, ErrDSGExactPreserve
	}
	marker, err := dsgOpenOwned(fd, dsgNSMarker, false)
	if err != nil {
		return i, ErrDSGExactPreserve
	}
	data, err := io.ReadAll(io.LimitReader(marker, 97))
	marker.Close()
	if err != nil || len(data) == 0 || len(data) > 96 {
		return i, ErrDSGExactPreserve
	}
	return dsgSameMountIdentity{BootSHA256: dsgRecoveryHash(bytes.TrimSpace(boot)), EnrollmentSHA256: dsgRecoveryHash(data),
		MountNS: ns.Ino, MountID: st.MountID, UniqueMountID: unique.Mnt_id, RootInode: st.Inode,
		Device: unix.Mkdev(st.DeviceMajor, st.DeviceMinor), FSID: fs.Fsid.Val}, nil
}

func dsgObserveSameMount(ctx context.Context, fd int, root string, source dsgSameMountSource, native dsgSameMountParticipant, readOnly bool) (dsgSameMountEvidence, error) {
	var e dsgSameMountEvidence
	if !source.valid() || native == nil || ctx.Err() != nil {
		return e, ErrDSGExactPreserve
	}
	pin, err := dsgPinSameMount(fd, root, readOnly)
	if err != nil {
		return e, err
	}
	record, err := native.evidence(ctx, fd, source)
	if err != nil || !record.valid() {
		return e, dsgSameMountFailure("record_observe", err)
	}
	again, err := dsgPinSameMount(fd, root, readOnly)
	if err != nil || pin != again || ctx.Err() != nil {
		return e, dsgSameMountFailure("record_repin", err)
	}
	return dsgSameMountEvidence{source, pin, pin.generation(), record}, nil
}

func dsgInspectSameMount(ctx context.Context, root string, source dsgSameMountSource, native dsgSameMountParticipant) (dsgSameMountEvidence, error) {
	fd, err := dsgLockRoot(root)
	if err != nil {
		return dsgSameMountEvidence{}, err
	}
	defer unix.Close(fd)
	return dsgObserveSameMount(ctx, fd, root, source, native, true)
}

type dsgSameMountAuthority struct {
	db     *bolt.DB
	device uint64
}

// The supervisor supplies a protected path off the target device. Bolt reuses
// the verified O_NOFOLLOW file descriptor, and creation syncs its parent entry.
func dsgOpenSameMountAuthority(path string) (*dsgSameMountAuthority, error) {
	parent := filepath.Dir(path)
	if !filepath.IsAbs(path) || filepath.Clean(path) != path || !dsgProtectedParent(path) {
		return nil, ErrDSGExactPreserve
	}
	dir, err := unix.Open(parent, unix.O_DIRECTORY|unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return nil, ErrDSGExactPreserve
	}
	defer unix.Close(dir)
	var pst unix.Stat_t
	if unix.Fstat(dir, &pst) != nil || int(pst.Uid) != os.Geteuid() || pst.Mode&07777 != 0700 {
		return nil, ErrDSGExactPreserve
	}
	var device uint64
	db, err := bolt.Open(path, 0600, &bolt.Options{Timeout: time.Second, OpenFile: func(_ string, flags int, _ os.FileMode) (*os.File, error) {
		fd, err := unix.Openat(dir, filepath.Base(path), flags|unix.O_NOFOLLOW|unix.O_CLOEXEC|unix.O_NONBLOCK, 0600)
		if err != nil {
			return nil, ErrDSGExactPreserve
		}
		var st unix.Stat_t
		if unix.Fstat(fd, &st) != nil || st.Mode&(unix.S_IFMT|07777) != unix.S_IFREG|0600 || int(st.Uid) != os.Geteuid() || st.Nlink != 1 || uint64(st.Dev) != uint64(pst.Dev) {
			unix.Close(fd)
			return nil, ErrDSGExactPreserve
		}
		device = uint64(st.Dev)
		return os.NewFile(uintptr(fd), path), nil
	}})
	if err != nil {
		return nil, ErrDSGExactPreserve
	}
	if device == 0 || unix.Fsync(dir) != nil {
		db.Close()
		return nil, ErrDSGExactPreserve
	}
	return &dsgSameMountAuthority{db, device}, nil
}

type dsgSameMountState struct {
	Grant   dsgSameMountGrant
	Phase   string
	Receipt *DSGExactReceipt
}

func (a *dsgSameMountAuthority) admissible(g dsgSameMountGrant) bool {
	return a != nil && a.db != nil && !a.db.NoSync && !a.db.IsReadOnly() && a.device != 0 && a.device != g.Evidence.Identity.Device && g.valid()
}

func (g dsgSameMountGrant) keys() (key []byte, claims [][]byte) {
	return []byte("approval/" + g.Request.Approval), [][]byte{[]byte("intent/" + g.Request.Intent), []byte("target/" + g.Evidence.Generation + "/" + g.Request.RecordID)}
}

// approve is a distinct supervisor-only admission operation. apply never creates
// its own grant, even if every field supplied by a caller is internally valid.
func (a *dsgSameMountAuthority) approve(ctx context.Context, g dsgSameMountGrant) error {
	if !a.admissible(g) {
		return ErrDSGExactPreserve
	}
	return a.db.Update(func(tx *bolt.Tx) error {
		b, err := tx.CreateBucketIfNotExists([]byte(dsgSameMountBucket))
		if err != nil {
			return err
		}
		key, claims := g.keys()
		if !g.active(ctx) || b.Get(key) != nil || b.Bucket(key) != nil {
			return ErrDSGExactPreserve
		}
		for _, claim := range claims {
			if b.Get(claim) != nil || b.Bucket(claim) != nil {
				return ErrDSGExactPreserve
			}
			if err := b.Put(claim, key); err != nil {
				return err
			}
		}
		raw, _ := json.Marshal(dsgSameMountState{Grant: g, Phase: "approved"})
		return b.Put(key, raw)
	})
}

// A failed preflight may leave exactly the original grant approved. Accept
// that one registration again while the separate operator proof is present.
func (a *dsgSameMountAuthority) admit(ctx context.Context, g dsgSameMountGrant) error {
	if !a.admissible(g) || !g.active(ctx) {
		return ErrDSGExactPreserve
	}
	err := a.db.View(func(tx *bolt.Tx) error {
		s, err := dsgReadSameMountState(tx, g)
		if err != nil || s.Phase != "approved" {
			return ErrDSGExactPreserve
		}
		return nil
	})
	if err == nil {
		return nil
	}
	return a.approve(ctx, g)
}

func dsgReadSameMountState(tx *bolt.Tx, g dsgSameMountGrant) (dsgSameMountState, error) {
	var s dsgSameMountState
	b := tx.Bucket([]byte(dsgSameMountBucket))
	key, claims := g.keys()
	if b == nil {
		return s, ErrDSGExactPreserve
	}
	for _, claim := range claims {
		if !bytes.Equal(b.Get(claim), key) {
			return s, ErrDSGExactPreserve
		}
	}
	raw := b.Get(key)
	if len(raw) == 0 || len(raw) > 16384 || json.Unmarshal(raw, &s) != nil || s.Grant != g {
		return dsgSameMountState{}, ErrDSGExactPreserve
	}
	canonical, _ := json.Marshal(s)
	if !bytes.Equal(canonical, raw) {
		return dsgSameMountState{}, ErrDSGExactPreserve
	}
	switch s.Phase {
	case "approved", "consumed":
		if s.Receipt == nil {
			return s, nil
		}
	case "settled":
		if s.Receipt != nil && g.matches(*s.Receipt) {
			return s, nil
		}
	}
	return dsgSameMountState{}, ErrDSGExactPreserve
}

func (a *dsgSameMountAuthority) advance(ctx context.Context, g dsgSameMountGrant, receipt *DSGExactReceipt) error {
	return a.db.Update(func(tx *bolt.Tx) error {
		s, err := dsgReadSameMountState(tx, g)
		if err != nil {
			return err
		}
		if receipt == nil {
			if s.Phase != "approved" || !g.active(ctx) {
				return ErrDSGExactPreserve
			}
			s.Phase = "consumed"
		} else {
			if s.Phase != "consumed" || ctx.Err() != nil || !g.matches(*receipt) {
				return ErrDSGExactPreserve
			}
			s.Phase = "settled"
			s.Receipt = receipt
		}
		key, _ := g.keys()
		raw, _ := json.Marshal(s)
		return tx.Bucket([]byte(dsgSameMountBucket)).Put(key, raw)
	})
}

type dsgSameMountRemount func(fd int, readOnly bool) error

// Reseal while the original flock is held, including when the remount reported
// an error after changing the superblock. Never redirect this callback to a
// replacement directory or mount. A failed reseal leaves a consumed grant.
func dsgResealSameMount(fd int, root string, expected dsgSameMountIdentity, remount dsgSameMountRemount) error {
	pin, err := dsgSameMountID(fd, root)
	if err != nil || pin != expected {
		return ErrDSGExactPreserve
	}
	if _, err = dsgFilesystem(fd, root, true); err != nil {
		if _, err = dsgFilesystem(fd, root, false); err != nil || remount == nil {
			return ErrDSGExactPreserve
		}
		if err = remount(fd, true); err != nil {
			return err
		}
	}
	after, err := dsgPinSameMount(fd, root, true)
	if err != nil {
		return err
	}
	if after != expected {
		return ErrDSGExactPreserve
	}
	return nil
}

// Phase labels are fixed at call sites. Do not include OS paths, object
// identifiers or raw errors from the stores in diagnostics.
type dsgSameMountPhaseError struct {
	phase string
	cause error
}

func (e dsgSameMountPhaseError) Error() string {
	label := "same_mount_" + e.phase
	switch cause := e.cause.(type) {
	case dsgSameMountPhaseError:
		return label + "/" + cause.Error()
	case dsgQuiescenceFailure:
		return label + "/quiescence_" + cause.reason
	case unix.Errno:
		// Numeric syscall errno is safe even when third-party errors include paths.
		return label + "/errno_" + strconv.FormatUint(uint64(cause), 10)
	}
	return label
}
func (e dsgSameMountPhaseError) Unwrap() error { return e.cause }
func dsgSameMountFailure(phase string, err error) error {
	if err == nil {
		err = ErrDSGExactPreserve
	}
	return dsgSameMountPhaseError{phase, err}
}

func (a *dsgSameMountAuthority) apply(ctx context.Context, root string, g dsgSameMountGrant, native dsgSameMountParticipant, remount dsgSameMountRemount) (DSGExactReceipt, error) {
	var empty DSGExactReceipt
	if !a.admissible(g) || !g.active(ctx) || native == nil || remount == nil {
		return empty, dsgSameMountFailure("grant", nil)
	}
	// Reject unknown, previously consumed or altered grants before considering a transition.
	if err := a.db.View(func(tx *bolt.Tx) error {
		s, err := dsgReadSameMountState(tx, g)
		if err != nil || s.Phase != "approved" {
			return ErrDSGExactPreserve
		}
		return nil
	}); err != nil {
		return empty, dsgSameMountFailure("registered_approval", err)
	}
	fd, err := dsgLockRoot(root)
	if err != nil {
		return empty, dsgSameMountFailure("root_lock", err)
	}
	defer unix.Close(fd)
	before, err := dsgObserveSameMount(ctx, fd, root, g.Evidence.Source, native, true)
	if err != nil {
		return empty, dsgSameMountFailure("ro_observe", err)
	}
	if before != g.Evidence {
		return empty, dsgSameMountFailure("ro_evidence", nil)
	}
	// Sync the one-shot claim before the first attempt to make the mount writable.
	if err = a.advance(ctx, g, nil); err != nil {
		return empty, dsgSameMountFailure("consume", err)
	}
	applied, err := func() (out DSGExactReceipt, result error) {
		defer func() {
			if sealErr := dsgResealSameMount(fd, root, before.Identity, remount); sealErr != nil {
				out, result = empty, errors.Join(result, dsgSameMountFailure("reseal", sealErr))
			}
		}()
		if err := remount(fd, false); err != nil {
			return empty, dsgSameMountFailure("make_writable", err)
		}
		rw, err := dsgObserveSameMount(ctx, fd, root, g.Evidence.Source, native, false)
		if err != nil {
			return empty, dsgSameMountFailure("rw_observe", err)
		}
		if rw != before || !g.active(ctx) {
			return empty, dsgSameMountFailure("rw_evidence", nil)
		}
		receipt, err := native.apply(ctx, fd, g)
		if err != nil {
			return empty, dsgSameMountFailure("native_apply", err)
		}
		return receipt, nil
	}()
	if err != nil {
		return empty, err
	}
	if !g.matches(applied) {
		return empty, dsgSameMountFailure("native_receipt_mismatch", nil)
	}
	return a.finish(ctx, root, fd, g, native, false)
}

func (a *dsgSameMountAuthority) finish(ctx context.Context, root string, fd int, g dsgSameMountGrant, native dsgSameMountParticipant, settled bool) (DSGExactReceipt, error) {
	var empty DSGExactReceipt
	got, err := native.receipt(ctx, fd, g)
	if err != nil || !g.matches(got) {
		return empty, dsgSameMountFailure("receipt_observe", err)
	}
	pin, err := dsgPinSameMount(fd, root, true)
	if err != nil {
		return empty, dsgSameMountFailure("receipt_pin", err)
	}
	if pin != g.Evidence.Identity || ctx.Err() != nil {
		return empty, dsgSameMountFailure("receipt_evidence", nil)
	}
	if !settled {
		if err = a.advance(ctx, g, &got); err != nil {
			return empty, dsgSameMountFailure("receipt_settle", err)
		}
	}
	return got, nil
}

// Recovery accepts an already consumed grant, including an expired one. It can
// reseal the original mount and confirm the native receipt. No participant can
// be asked to start or repeat an effect from this entry point.
func (a *dsgSameMountAuthority) recover(ctx context.Context, root string, g dsgSameMountGrant, native dsgSameMountParticipant, remount dsgSameMountRemount) (DSGExactReceipt, error) {
	var empty DSGExactReceipt
	if !a.admissible(g) || ctx.Err() != nil || native == nil {
		return empty, ErrDSGExactPreserve
	}
	fd, err := dsgLockRoot(root)
	if err != nil {
		return empty, err
	}
	defer unix.Close(fd)
	var s dsgSameMountState
	err = a.db.View(func(tx *bolt.Tx) error {
		var err error
		s, err = dsgReadSameMountState(tx, g)
		return err
	})
	if err != nil || s.Phase == "approved" {
		return empty, ErrDSGExactPreserve
	}
	if err = dsgResealSameMount(fd, root, g.Evidence.Identity, remount); err != nil {
		return empty, err
	}
	return a.finish(ctx, root, fd, g, native, s.Phase == "settled")
}
