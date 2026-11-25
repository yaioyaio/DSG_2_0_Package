//go:build linux

package cache

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"time"

	"github.com/moby/buildkit/cache/metadata"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

const dsgMarker = ".dsg-dedicated-worker-v1"

// DSGRejectEnrolledRoot keeps a flagless invocation of the patched binary out
// of an enrolled worker. The daemon repeats it inside its ordinary startup lock.
func DSGRejectEnrolledRoot(root string) error {
	for _, name := range []string{dsgMarker, dsgNSMarker} {
		if _, err := os.Lstat(filepath.Join(root, name)); !errors.Is(err, os.ErrNotExist) {
			return ErrDSGExactPreserve
		}
	}
	return nil
}

func dsgValidMarker(dir int) bool {
	var st unix.Stat_t
	return unix.Fstatat(dir, dsgMarker, &st, unix.AT_SYMLINK_NOFOLLOW) == nil &&
		st.Mode&(unix.S_IFMT|07777) == unix.S_IFREG|0600 && int(st.Uid) == os.Geteuid() && st.Nlink == 1 && st.Size == 0
}

// DSGAcquireDedicatedDaemon enrolls only a fresh empty private root. A daemon
// owns this raw O_CLOEXEC descriptor until the kernel closes it at process exit,
// including while upstream shutdown callbacks and background goroutines run.
func DSGAcquireDedicatedDaemon(root string) error {
	return dsgAcquireDaemon(root, false)
}

func DSGAcquireQuiescentDaemon(root string) error {
	return dsgAcquireDaemon(root, true)
}

func dsgAcquireDaemon(root string, strict bool) error {
	return dsgAcquireDaemonFromProc(root, strict, "/proc")
}

func dsgAcquireDaemonFromProc(root string, strict bool, procRoot string) error {
	fd, err := dsgLockRoot(root)
	if err != nil {
		return err
	}
	var st unix.Stat_t
	if !errors.Is(unix.Fstatat(fd, dsgNSMarker, &st, unix.AT_SYMLINK_NOFOLLOW), unix.ENOENT) {
		unix.Close(fd)
		return dsgQError("enrollment")
	}
	if dsgValidMarker(fd) {
		if strict {
			unix.Close(fd)
			return dsgQError("enrollment")
		}
		return nil
	}
	// The duplicate shares the read offset but does not release the lease.
	dup, err := unix.FcntlInt(uintptr(fd), unix.F_DUPFD_CLOEXEC, 0)
	if err != nil {
		unix.Close(fd)
		return ErrDSGExactPreserve
	}
	listing := os.NewFile(uintptr(dup), root)
	_, err = listing.Readdirnames(1)
	listing.Close()
	if !errors.Is(err, io.EOF) {
		unix.Close(fd)
		return ErrDSGExactPreserve
	}
	if strict {
		if err := dsgEnrollNamespace(fd, root, procRoot); err != nil {
			unix.Close(fd)
			return err
		}
	}
	marker, err := unix.Openat(fd, dsgMarker, unix.O_CREAT|unix.O_EXCL|unix.O_NOFOLLOW|unix.O_WRONLY|unix.O_CLOEXEC, 0600)
	if err != nil {
		unix.Close(fd)
		return ErrDSGExactPreserve
	}
	err = unix.Fsync(marker)
	unix.Close(marker)
	if err != nil || unix.Fsync(fd) != nil || !dsgValidMarker(fd) {
		unix.Close(fd)
		return ErrDSGExactPreserve
	}
	return nil
}

// The native snapshot and build history DBs may be absent.
type DSGDedicatedObservation struct {
	Worker       string                  `json:"worker"`
	SolverTx     int                     `json:"solver_tx"`
	CacheTx      int                     `json:"cache_tx"`
	ContainerdTx int                     `json:"containerd_tx"`
	SnapshotTx   *int                    `json:"snapshot_tx"`
	HistoryTx    *int                    `json:"history_tx,omitempty"`
	Quiescence   *DSGDedicatedQuiescence `json:"quiescence,omitempty"`
	Graph        *DSGDedicatedGraph      `json:"graph,omitempty"`
	Record       *DSGDedicatedRecord     `json:"record,omitempty"`
}

// These are persisted binding observations. Metadata alone cannot qualify
// a native worker for deletion or prove that the underlying allocations exist.
type DSGDedicatedRecord struct {
	ID                    string `json:"id"`
	RecordPresent         bool   `json:"record_present"`
	SnapshotBinding       bool   `json:"snapshot_binding"`
	MutableAlias          bool   `json:"mutable_alias"`
	BlobBinding           bool   `json:"blob_binding"`
	BindingsSHA256        string `json:"bindings_sha256,omitempty"`
	MetadataOnlyCandidate bool   `json:"metadata_only_candidate"`
}

func dsgBinding(bucket *bolt.Bucket, key string) (string, error) {
	if bucket.Bucket([]byte(key)) != nil {
		return "", ErrDSGExactPreserve
	}
	raw := bucket.Get([]byte(key))
	// v0.35.1 StorageItem.SetValue(nil) writes a zero-length value. Finalize
	// clears equalMutable this way; newStorageItem also skips empty values.
	if len(raw) == 0 {
		return "", nil
	}
	var v metadata.Value
	var value *string
	if len(raw) > 1024 || json.Unmarshal(raw, &v) != nil || v.Unmarshal(&value) != nil || value == nil || len(*value) > 256 {
		return "", ErrDSGExactPreserve
	}
	return *value, nil
}

func dsgObserveRecord(tx *bolt.Tx, worker, id string) (*DSGDedicatedRecord, error) {
	record := &DSGDedicatedRecord{ID: id}
	main := tx.Bucket([]byte(dsgMain))
	if main == nil {
		return record, nil
	}
	b := main.Bucket([]byte(id))
	if b == nil {
		if main.Get([]byte(id)) != nil {
			return nil, ErrDSGExactPreserve
		}
		return record, nil
	}
	record.RecordPresent = true
	snapshot, err := dsgBinding(b, keySnapshot)
	if err != nil {
		return nil, err
	}
	alias, err := dsgBinding(b, keyEqualMutable)
	if err != nil {
		return nil, err
	}
	blob, err := dsgBinding(b, keyBlob)
	if err != nil {
		return nil, err
	}
	record.SnapshotBinding = snapshot != ""
	record.BlobBinding = blob != ""
	record.MutableAlias = alias != ""
	// Hash only the worker/record IDs and the three classified native fields.
	// No free-form descriptions, image references or external data are read.
	// JSON string-array framing preserves boundaries, and Bolt iterates incoming
	// alias endpoints in sorted order.
	bindings := []string{"dsg-native-bindings-v1", worker, id, snapshot, alias, blob}
	count := 0
	err = main.ForEach(func(k, v []byte) error {
		count++
		if count > 4096 || v != nil || !dsgFullID.Match(k) {
			return ErrDSGExactPreserve
		}
		if string(k) == id {
			return nil
		}
		otherAlias, err := dsgBinding(main.Bucket(k), keyEqualMutable)
		if err != nil {
			return err
		}
		if otherAlias == id {
			record.MutableAlias = true
			bindings = append(bindings, string(k))
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	data, err := json.Marshal(bindings)
	if err != nil {
		return nil, err
	}
	sum := sha256.Sum256(data)
	record.BindingsSHA256 = hex.EncodeToString(sum[:])
	return record, nil
}

func dsgOpenOwned(dir int, name string, directory bool) (*os.File, error) {
	flags := unix.O_RDONLY | unix.O_NOFOLLOW | unix.O_CLOEXEC | unix.O_NONBLOCK
	if directory {
		flags |= unix.O_DIRECTORY
	}
	fd, err := unix.Openat(dir, name, flags, 0)
	if err != nil {
		return nil, err
	}
	var st unix.Stat_t
	if unix.Fstat(fd, &st) != nil || int(st.Uid) != os.Geteuid() || st.Mode&0022 != 0 ||
		(!directory && (st.Mode&unix.S_IFMT != unix.S_IFREG || st.Nlink != 1)) {
		unix.Close(fd)
		return nil, ErrDSGExactPreserve
	}
	return os.NewFile(uintptr(fd), name), nil
}

// DSGInspectDedicatedWorker runs offline. It never starts a cache manager,
// solver, containerd DB service or snapshotter (their constructors can mutate).
func DSGInspectDedicatedWorker(root string) (DSGDedicatedObservation, error) {
	return dsgInspectDedicated(root, "", false)
}

func DSGInspectDedicatedRecord(root, id string) (DSGDedicatedObservation, error) {
	if !dsgFullID.MatchString(id) {
		return DSGDedicatedObservation{}, ErrDSGExactPreserve
	}
	return dsgInspectDedicated(root, id, false)
}

func DSGInspectQuiescentDedicatedRecord(root, id string) (DSGDedicatedObservation, error) {
	if !dsgFullID.MatchString(id) {
		return DSGDedicatedObservation{}, ErrDSGExactPreserve
	}
	return dsgInspectDedicated(root, id, true)
}

func dsgInspectDedicated(root, recordID string, strict bool) (DSGDedicatedObservation, error) {
	var o DSGDedicatedObservation
	dir, err := dsgLockRoot(root)
	if err != nil {
		return o, err
	}
	defer unix.Close(dir)
	if !dsgValidMarker(dir) {
		return o, ErrDSGExactPreserve
	}
	if strict {
		if err := dsgCheckQuiescence(dir, root); err != nil {
			return o, err
		}
	}
	o, err = dsgInspectPinned(dir, recordID)
	if err != nil {
		return DSGDedicatedObservation{}, err
	}
	if strict {
		if err := dsgCheckQuiescence(dir, root); err != nil {
			return DSGDedicatedObservation{}, err
		}
		o.Quiescence = &DSGDedicatedQuiescence{true, true, true, true}
	}
	return o, nil
}

func dsgInspectPinned(dir int, recordID string) (DSGDedicatedObservation, error) {
	var o DSGDedicatedObservation
	worker, err := dsgOpenOwned(dir, "runc-native", true)
	if err != nil {
		return o, ErrDSGExactPreserve
	}
	defer worker.Close()
	workerFD := int(worker.Fd())
	sn, err := dsgOpenOwned(workerFD, "snapshots", true)
	if err != nil {
		return o, ErrDSGExactPreserve
	}
	defer sn.Close()
	id, err := dsgOpenOwned(workerFD, "workerid", false)
	if err != nil {
		return o, ErrDSGExactPreserve
	}
	data, err := io.ReadAll(io.LimitReader(id, 26))
	id.Close()
	if err != nil || !dsgFullID.Match(data) {
		return o, ErrDSGExactPreserve
	}
	o.Worker = string(data)
	var snapTx, historyTx int
	var txs [5]*bolt.Tx
	for i, dbSpec := range []struct {
		dir  int
		name string
		tx   *int
	}{
		{dir, "cache.db", &o.SolverTx}, {workerFD, "metadata_v2.db", &o.CacheTx},
		{workerFD, "containerdmeta.db", &o.ContainerdTx}, {int(sn.Fd()), "metadata.db", &snapTx},
		{dir, "history.db", &historyTx},
	} {
		if dbSpec.tx == &historyTx && recordID == "" {
			continue
		}
		// Supply the pinned, non-symlink descriptor. No path is reopened for a
		// compaction, creation, initialization, or sidecar DB.
		db, err := bolt.Open(dbSpec.name, 0400, &bolt.Options{ReadOnly: true, Timeout: 100 * time.Millisecond,
			OpenFile: func(_ string, _ int, _ os.FileMode) (*os.File, error) {
				file, err := dsgOpenOwned(dbSpec.dir, dbSpec.name, false)
				if err != nil {
					return nil, err
				}
				info, err := file.Stat()
				if err != nil || info.Size() < 8192 || info.Size() > 128<<20 {
					file.Close()
					return nil, ErrDSGExactPreserve
				}
				return file, nil
			},
		})
		if err != nil {
			if (dbSpec.tx == &snapTx || dbSpec.tx == &historyTx) && errors.Is(err, os.ErrNotExist) {
				continue
			}
			return DSGDedicatedObservation{}, ErrDSGExactPreserve
		}
		defer db.Close()
		tx, err := db.Begin(false)
		if err != nil {
			return DSGDedicatedObservation{}, ErrDSGExactPreserve
		}
		defer tx.Rollback()
		txs[i] = tx
		*dbSpec.tx = tx.ID()
		if dbSpec.tx == &snapTx {
			o.SnapshotTx = &snapTx
		}
		if dbSpec.tx == &historyTx {
			o.HistoryTx = &historyTx
		}
	}
	if recordID != "" {
		o.Record, err = dsgObserveRecord(txs[1], o.Worker, recordID)
		if err != nil {
			return DSGDedicatedObservation{}, ErrDSGExactPreserve
		}
		if o.Record.RecordPresent {
			ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
			defer cancel()
			o.Graph, err = dsgObserveNativeGraph(ctx, o.Worker, recordID, txs[0], txs[1], txs[2], txs[3], workerFD, int(sn.Fd()), txs[4])
			if err != nil {
				return DSGDedicatedObservation{}, ErrDSGExactPreserve
			}
		}
	}
	return o, nil
}
