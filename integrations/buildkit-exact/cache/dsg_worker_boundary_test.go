//go:build linux || darwin

package cache

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/moby/buildkit/cache/metadata"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

const (
	dsgCrashPhaseEnv    = "DSG_EXACT_TEST_CRASH_PHASE"
	dsgCrashRootEnv     = "DSG_EXACT_TEST_CRASH_ROOT"
	dsgCrashUncommitted = "before-commit"
	dsgCrashCommitted   = "after-commit"
	dsgCrashContention  = "write-contention"
	dsgBeforeCommitExit = 81
	dsgAfterCommitExit  = 82
	dsgContentionExit   = 83
)

type dsgCrashInventory struct {
	Grant                                            DSGExactIntent
	Leases, Snapshots, Content, Solver, ExternalRefs []string
}

type dsgCrashFixture struct {
	root, dbPath, inventoryPath string
	intent                      DSGExactIntent
	nonTargetDigest             string
}

func dsgSeedCrashWorker(t *testing.T) dsgCrashFixture {
	t.Helper()
	f := dsgNewFixture(t)
	root := filepath.Dir(f.path)
	require.NoError(t, os.Chmod(root, 0700))
	nonTargetDigest, err := f.api.DSGExactMetadataDigest(dsgOther)
	require.NoError(t, err)
	require.NoError(t, f.api.manager.MetadataStore.Close())
	i := f.fence.inventory
	data, err := json.Marshal(dsgCrashInventory{
		Grant: i.grant, Leases: i.leases, Snapshots: i.snapshots, Content: i.content,
		Solver: i.solver, ExternalRefs: i.externalRefs,
	})
	require.NoError(t, err)
	inventoryPath := filepath.Join(root, "inventory.json")
	require.NoError(t, os.WriteFile(inventoryPath, data, 0600))
	return dsgCrashFixture{
		root: root, dbPath: f.path, inventoryPath: inventoryPath, intent: i.grant, nonTargetDigest: nonTargetDigest,
	}
}

func dsgReadCrashInventory(root string) (dsgFrozenInventory, error) {
	data, err := os.ReadFile(filepath.Join(root, "inventory.json"))
	if err != nil {
		return dsgFrozenInventory{}, err
	}
	var i dsgCrashInventory
	if err := json.Unmarshal(data, &i); err != nil {
		return dsgFrozenInventory{}, err
	}
	return dsgFrozenInventory{grant: i.Grant, leases: i.Leases, snapshots: i.Snapshots,
		content: i.Content, solver: i.Solver, externalRefs: i.ExternalRefs}, nil
}

// Rebuild the synthetic worker's in-memory view from native Bolt metadata on
// both sides of the interruption. No backing object or daemon is attached.
func dsgOpenCrashWorker(t *testing.T, root string) *DSGExactAPI {
	t.Helper()
	path := filepath.Join(root, "cache.db")
	file, err := os.Lstat(path)
	require.NoError(t, err)
	require.True(t, file.Mode().IsRegular())
	store, err := metadata.NewStore(path)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, store.Close()) })
	manager := &cacheManager{records: make(map[string]*cacheRecord), MetadataStore: store}
	items, err := store.All()
	require.NoError(t, err)
	for _, item := range items {
		manager.records[item.ID()] = &cacheRecord{
			mu: &sync.Mutex{}, cm: manager, cacheMetadata: &cacheMetadata{si: item},
		}
	}
	api, err := NewDSGExactAPI(manager)
	require.NoError(t, err)
	api.fence = &dsgDedicatedFence{root: root, inspect: func(ctx context.Context) (dsgFrozenInventory, error) {
		if err := ctx.Err(); err != nil {
			return dsgFrozenInventory{}, err
		}
		return dsgReadCrashInventory(root)
	}}
	return api
}

func dsgRunCrashChild(t *testing.T, root, phase string, wantExit int) {
	t.Helper()
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 8*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, executable, "-test.run=^TestDSGExactDedicatedWorkerCrash$")
	cmd.Env = []string{dsgCrashPhaseEnv + "=" + phase, dsgCrashRootEnv + "=" + root}
	output, err := cmd.CombinedOutput()
	var exit *exec.ExitError
	if ctx.Err() != nil || !errors.As(err, &exit) || exit.ExitCode() != wantExit {
		t.Fatalf("child did not exit at %s: %v (%v)\n%s", phase, err, ctx.Err(), output)
	}
}

func dsgWorkerCrashChild(t *testing.T, root, phase string) {
	t.Helper()
	require.NotEmpty(t, root)
	i, err := dsgReadCrashInventory(root)
	require.NoError(t, err)
	require.True(t, i.grant.valid())
	require.Equal(t, dsgTarget, i.grant.RecordID)
	api := dsgOpenCrashWorker(t, root)
	write := func() error {
		t.Fatal("contending writer entered the dedicated root")
		return nil
	}
	interrupt := func(exitCode int) func() {
		return func() {
			require.ErrorIs(t, DSGWithDedicatedMaintenance(root, write), ErrDSGExactPreserve)
			os.Exit(exitCode)
		}
	}
	var beforeCommit, afterCommit func()
	switch phase {
	case dsgCrashUncommitted:
		beforeCommit = interrupt(dsgBeforeCommitExit)
	case dsgCrashCommitted:
		afterCommit = interrupt(dsgAfterCommitExit)
	case dsgCrashContention:
		require.ErrorIs(t, DSGWithDedicatedWorker(root, write), ErrDSGExactPreserve)
		require.ErrorIs(t, DSGWithDedicatedMaintenance(root, write), ErrDSGExactPreserve)
		_, err := api.Apply(t.Context(), i.grant)
		require.ErrorIs(t, err, ErrDSGExactPreserve)
		os.Exit(dsgContentionExit)
	default:
		t.Fatalf("unknown child phase %q", phase)
	}
	_, err = api.dsgApply(t.Context(), i.grant, beforeCommit, afterCommit)
	t.Fatalf("apply returned before injected process exit: %v", err)
}

func dsgCrashFileDigest(t *testing.T, path string) [sha256.Size]byte {
	t.Helper()
	data, err := os.ReadFile(path)
	require.NoError(t, err)
	return sha256.Sum256(data)
}

func TestDSGExactDedicatedWorkerCrash(t *testing.T) {
	if phase := os.Getenv(dsgCrashPhaseEnv); phase != "" {
		dsgWorkerCrashChild(t, os.Getenv(dsgCrashRootEnv), phase)
		return
	}
	for _, tc := range []struct {
		phase     string
		exitCode  int
		committed bool
	}{
		{dsgCrashUncommitted, dsgBeforeCommitExit, false},
		{dsgCrashCommitted, dsgAfterCommitExit, true},
	} {
		t.Run(tc.phase, func(t *testing.T) {
			first, second := dsgSeedCrashWorker(t), dsgSeedCrashWorker(t)
			beforeMetadata := dsgCrashFileDigest(t, first.dbPath)
			beforeInventory := dsgCrashFileDigest(t, first.inventoryPath)
			secondMetadata := dsgCrashFileDigest(t, second.dbPath)
			secondInventory := dsgCrashFileDigest(t, second.inventoryPath)
			require.Equal(t, first.intent.RecordID, second.intent.RecordID)
			dsgRunCrashChild(t, first.root, tc.phase, tc.exitCode)

			api := dsgOpenCrashWorker(t, first.root)
			other, err := api.DSGExactMetadataDigest(dsgOther)
			require.NoError(t, err)
			require.Equal(t, first.nonTargetDigest, other)
			require.Equal(t, beforeInventory, dsgCrashFileDigest(t, first.inventoryPath))
			require.Equal(t, secondMetadata, dsgCrashFileDigest(t, second.dbPath))
			require.Equal(t, secondInventory, dsgCrashFileDigest(t, second.inventoryPath))

			var consumed []byte
			require.NoError(t, api.manager.MetadataStore.DB().View(func(tx *bolt.Tx) error {
				if bucket := tx.Bucket([]byte(dsgConsumed)); bucket != nil {
					consumed = append(consumed, bucket.Get([]byte(first.intent.Approval))...)
				}
				return nil
			}))
			target, targetErr := DSGInspectExact(t.Context(), api.manager, first.intent.RecordID)
			receipt, receiptErr := api.Receipt(first.intent)
			if !tc.committed {
				require.NoError(t, targetErr)
				require.Equal(t, first.intent.RecordID, target.RecordID)
				require.ErrorIs(t, receiptErr, ErrDSGExactPreserve)
				require.Empty(t, consumed)
				require.Equal(t, beforeMetadata, dsgCrashFileDigest(t, first.dbPath))
			} else {
				require.ErrorIs(t, targetErr, ErrDSGExactPreserve)
				require.NoError(t, receiptErr)
				require.Equal(t, DSGExactReceipt{Request: first.intent, LogicalRecords: 1}, receipt)
				require.Equal(t, first.intent.Intent, string(consumed))
				committedMetadata := dsgCrashFileDigest(t, first.dbPath)
				_, err := api.Apply(t.Context(), first.intent)
				require.ErrorIs(t, err, ErrDSGExactPreserve)
				recovered, err := api.Receipt(first.intent)
				require.NoError(t, err)
				require.Equal(t, receipt, recovered)
				require.Equal(t, committedMetadata, dsgCrashFileDigest(t, first.dbPath))
			}
			// Both abrupt exits must release the same lock used by maintenance.
			maintained := false
			require.NoError(t, DSGWithDedicatedMaintenance(first.root, func() error {
				maintained = true
				return nil
			}))
			require.True(t, maintained)
		})
	}
}

func TestDSGExactDedicatedWorkerBoundary(t *testing.T) {
	first, second := dsgSeedCrashWorker(t), dsgSeedCrashWorker(t)
	before := dsgCrashFileDigest(t, first.dbPath)
	parentAlias := filepath.Join(t.TempDir(), "parent-link")
	require.NoError(t, os.Symlink(filepath.Dir(first.root), parentAlias))
	aliasedRoot := filepath.Join(parentAlias, filepath.Base(first.root))
	write := func() error {
		t.Error("contending writer entered the dedicated root")
		return nil
	}
	independent := false
	require.NoError(t, DSGWithDedicatedWorker(first.root, func() error {
		require.ErrorIs(t, DSGWithDedicatedMaintenance(first.root, write), ErrDSGExactPreserve)
		require.ErrorIs(t, DSGWithDedicatedMaintenance(aliasedRoot, write), ErrDSGExactPreserve)
		dsgRunCrashChild(t, first.root, dsgCrashContention, dsgContentionExit)
		return DSGWithDedicatedWorker(second.root, func() error {
			independent = true
			return nil
		})
	}))
	require.True(t, independent)
	require.NoError(t, DSGWithDedicatedMaintenance(first.root, func() error {
		require.ErrorIs(t, DSGWithDedicatedWorker(first.root, write), ErrDSGExactPreserve)
		return nil
	}))
	require.Equal(t, before, dsgCrashFileDigest(t, first.dbPath))
}
