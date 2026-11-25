//go:build linux

package cache

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

func TestDSGExactDedicatedOfflineInspection(t *testing.T) {
	root := t.TempDir()
	require.NoError(t, os.Chmod(root, 0700))
	worker := filepath.Join(root, "runc-native")
	snapshots := filepath.Join(worker, "snapshots")
	require.NoError(t, os.MkdirAll(snapshots, 0700))
	require.NoError(t, os.WriteFile(filepath.Join(worker, "workerid"), []byte(dsgTarget), 0600))
	require.ErrorIs(t, DSGAcquireDedicatedDaemon(root), ErrDSGExactPreserve, "do not enroll a nonempty root")
	_, err := DSGInspectDedicatedWorker(root)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	marker := filepath.Join(root, dsgMarker)
	require.NoError(t, os.WriteFile(marker, nil, 0600))
	require.ErrorIs(t, DSGRejectEnrolledRoot(root), ErrDSGExactPreserve)
	backend := filepath.Join(worker, "containerdmeta.db")
	createDB := func(path string) int {
		t.Helper()
		db, err := bolt.Open(path, 0600, nil)
		require.NoError(t, err)
		var id int
		require.NoError(t, db.Update(func(tx *bolt.Tx) error {
			id = tx.ID()
			_, err := tx.CreateBucket([]byte("fixture"))
			return err
		}))
		require.NoError(t, db.Close())
		return id
	}
	want := DSGDedicatedObservation{Worker: dsgTarget, SolverTx: createDB(filepath.Join(root, "cache.db")),
		CacheTx: createDB(filepath.Join(worker, "metadata_v2.db")), ContainerdTx: createDB(backend)}
	observation, err := DSGInspectDedicatedWorker(root)
	require.NoError(t, err)
	require.Equal(t, want, observation)
	snapID := createDB(filepath.Join(snapshots, "metadata.db"))
	want.SnapshotTx = &snapID
	observation, err = DSGInspectDedicatedWorker(root)
	require.NoError(t, err)
	require.Equal(t, want, observation)
	for _, badID := range []string{"", "abc", dsgTarget + "a"} {
		_, err := DSGInspectDedicatedRecord(root, badID)
		require.ErrorIs(t, err, ErrDSGExactPreserve)
	}
	missing, err := DSGInspectDedicatedRecord(root, dsgTarget)
	require.NoError(t, err)
	require.Equal(t, &DSGDedicatedRecord{ID: dsgTarget}, missing.Record)
	cacheDB, err := bolt.Open(filepath.Join(worker, "metadata_v2.db"), 0600, nil)
	require.NoError(t, err)
	require.NoError(t, cacheDB.Update(func(tx *bolt.Tx) error {
		main, err := tx.CreateBucket([]byte(dsgMain))
		if err != nil {
			return err
		}
		target, err := main.CreateBucket([]byte(dsgTarget))
		if err != nil {
			return err
		}
		// Native Finalize clears its outward alias with Put(key,nil).
		// The peer below still has an incoming binding to this ID.
		if err := target.Put([]byte(keyEqualMutable), nil); err != nil {
			return err
		}
		if err := target.Put([]byte(keySnapshot), []byte(`{"value":"synthetic-snapshot"}`)); err != nil {
			return err
		}
		if err := target.Put([]byte(keyBlob), []byte(`{"value":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}`)); err != nil {
			return err
		}
		peer, err := main.CreateBucket([]byte(dsgOther))
		if err != nil {
			return err
		}
		return peer.Put([]byte(keyEqualMutable), []byte(`{"value":"`+dsgTarget+`"}`))
	}))
	require.NoError(t, cacheDB.Close())
	history := filepath.Join(root, "history.db")
	historyTx := createDB(history)
	exact, err := DSGInspectDedicatedRecord(root, dsgTarget)
	require.NoError(t, err)
	require.Equal(t, &historyTx, exact.HistoryTx)
	require.True(t, exact.Record.RecordPresent)
	require.True(t, exact.Record.SnapshotBinding)
	require.True(t, exact.Record.BlobBinding)
	require.True(t, exact.Record.MutableAlias)
	require.False(t, exact.Record.MetadataOnlyCandidate)
	require.Regexp(t, "^[0-9a-f]{64}$", exact.Record.BindingsSHA256)
	peer, err := DSGInspectDedicatedRecord(root, dsgOther)
	require.NoError(t, err)
	require.True(t, peer.Record.MutableAlias)
	require.False(t, peer.Record.SnapshotBinding)
	require.NotEqual(t, exact.Record.BindingsSHA256, peer.Record.BindingsSHA256)
	repeat, err := DSGInspectDedicatedRecord(root, dsgTarget)
	require.NoError(t, err)
	require.Equal(t, exact, repeat)
	require.NoError(t, os.Link(history, filepath.Join(t.TempDir(), "outside-history.db")))
	_, err = DSGInspectDedicatedRecord(root, dsgTarget)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	// An uncoordinated DB writer must still be detected by the read-only open.
	writer, err := bolt.Open(backend, 0600, nil)
	require.NoError(t, err)
	_, err = DSGInspectDedicatedWorker(root)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.NoError(t, writer.Close())
	// A database linked outside the owned worker no longer belongs to this scope.
	require.NoError(t, os.Link(backend, filepath.Join(t.TempDir(), "outside.db")))
	_, err = DSGInspectDedicatedWorker(root)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
}
