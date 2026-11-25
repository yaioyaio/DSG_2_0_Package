//go:build linux

package cache

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

// All perturbations affect temporary supervisor files containing synthetic grants.
// No fixture opens a worker or requests a native effect.
func TestDSGLedgerLossRecovery(t *testing.T) {
	ctx := t.Context()
	for _, fault := range []string{"missing", "truncated", "partial", "copied", "approved-rollback", "missing-checkpoint", "torn-checkpoint", "restore-settled"} {
		t.Run(fault, func(t *testing.T) {
			stage := t.TempDir()
			require.NoError(t, os.Chmod(stage, 0700))
			path := filepath.Join(stage, "grants.db")
			a, err := dsgCreateSameMountAuthority(path)
			require.NoError(t, err)
			g := dsgTestLedgerGrant(a.device + 1)
			require.NoError(t, a.approve(ctx, g))
			approved, err := os.ReadFile(path)
			require.NoError(t, err)
			require.NoError(t, a.advance(ctx, g, nil))
			receipt := DSGExactReceipt{Request: g.Request, LogicalRecords: 1}
			require.NoError(t, a.advance(ctx, g, &receipt))
			require.NoError(t, a.db.Close())
			settled, err := os.ReadFile(path)
			require.NoError(t, err)
			witness, err := os.ReadFile(path + ".checkpoint")
			require.NoError(t, err)

			switch fault {
			case "missing":
				require.NoError(t, os.Rename(path, path+".saved"))
			case "truncated":
				require.NoError(t, os.Truncate(path, 0))
			case "partial":
				require.NoError(t, os.Truncate(path, int64(len(settled)/2)))
			case "copied":
				require.NoError(t, os.Rename(path, path+".saved"))
				require.NoError(t, os.WriteFile(path, settled, 0600))
			case "approved-rollback":
				require.NoError(t, os.WriteFile(path, approved, 0600))
			case "missing-checkpoint":
				require.NoError(t, os.Rename(path+".checkpoint", path+".saved"))
			case "torn-checkpoint":
				require.NoError(t, os.Truncate(path+".checkpoint", 20))
			case "restore-settled":
				// Restoring the original contents to the very same inode agrees
				// with the surviving independent checkpoint and leaves it spent.
				require.NoError(t, os.WriteFile(path, approved, 0600))
				require.NoError(t, os.WriteFile(path, settled, 0600))
			}
			before, beforeErr := os.ReadFile(path)
			reopened, err := dsgOpenSameMountAuthority(path)
			if fault == "restore-settled" {
				require.NoError(t, err)
				defer reopened.db.Close()
				require.ErrorIs(t, reopened.admit(ctx, g), ErrDSGExactPreserve)
				require.ErrorIs(t, reopened.advance(ctx, g, nil), ErrDSGExactPreserve)
				require.NoError(t, reopened.db.View(func(tx *bolt.Tx) error {
					state, err := dsgReadSameMountState(tx, g)
					require.Equal(t, "settled", state.Phase)
					require.Equal(t, &receipt, state.Receipt)
					return err
				}))
				afterWitness, err := os.ReadFile(path + ".checkpoint")
				require.NoError(t, err)
				require.Equal(t, witness, afterWitness)
			} else {
				require.ErrorIs(t, err, ErrDSGExactPreserve)
				require.Nil(t, reopened)
				if fault == "missing" {
					reinitialized, err := dsgCreateSameMountAuthority(path)
					require.ErrorIs(t, err, ErrDSGExactPreserve)
					require.Nil(t, reinitialized)
				}
			}
			after, afterErr := os.ReadFile(path)
			if errors.Is(beforeErr, os.ErrNotExist) {
				require.ErrorIs(t, afterErr, os.ErrNotExist)
			} else {
				require.NoError(t, beforeErr)
				require.NoError(t, afterErr)
				require.Equal(t, before, after)
			}
		})
	}
}

// The stage is volatile and the witness sits on a separate filesystem. Use
// only new synthetic files; neither the worker nor its cache is opened.
func TestDSGLedgerIndependentSettledCheckpoint(t *testing.T) {
	base := t.TempDir()
	require.NoError(t, os.Chmod(base, 0700))
	registration := filepath.Join(base, "registration")
	require.NoError(t, os.Mkdir(registration, 0700))
	stage, err := os.MkdirTemp("/dev/shm", "dsg-ledger-")
	require.NoError(t, err)
	path := filepath.Join(stage, "grants.db")
	t.Cleanup(func() {
		for _, name := range []string{path + ".checkpoint", path, stage} {
			if err := os.Remove(name); err != nil && !errors.Is(err, os.ErrNotExist) {
				t.Error(err)
			}
		}
	})
	authority, err := dsgCreateSameMountAuthority(path)
	require.NoError(t, err)
	ledgerDevice := authority.device
	require.NoError(t, authority.db.Close())
	read := func(path string) []byte {
		t.Helper()
		raw, err := os.ReadFile(path)
		require.NoError(t, err)
		return raw
	}
	w := &dsgLedgerWitness{path: filepath.Join(registration, "ledger.sha256")}
	require.NoError(t, os.WriteFile(w.path, read(path+".checkpoint"), 0600))
	var st unix.Stat_t
	require.NoError(t, unix.Stat(w.path, &st))
	w.identity = dsgWitnessIdentity{uint64(st.Dev), st.Ino}
	w.workerDevice = max(ledgerDevice, w.identity.Device) + 1
	open := func() (*dsgSameMountAuthority, error) { return dsgAccessSameMountAuthority(path, false, w) }
	authority, err = open()
	require.NoError(t, err)
	t.Cleanup(func() {
		if authority != nil {
			authority.db.Close()
		}
	})
	grant := dsgTestLedgerGrant(w.workerDevice)
	require.NoError(t, authority.approve(t.Context(), grant))
	approvedDB, approvedCheckpoint := read(path), read(path+".checkpoint")
	require.Equal(t, approvedCheckpoint, read(w.path))
	require.NoError(t, authority.advance(t.Context(), grant, nil))
	consumed := read(w.path)
	require.Equal(t, read(path+".checkpoint"), consumed)
	require.NotEqual(t, approvedCheckpoint, consumed)
	receipt := DSGExactReceipt{Request: grant.Request, LogicalRecords: 1}
	require.NoError(t, authority.advance(t.Context(), grant, &receipt))
	settled := read(w.path)
	require.Equal(t, read(path+".checkpoint"), settled)
	require.NotEqual(t, consumed, settled)
	require.NoError(t, authority.db.Close())
	authority = nil
	// A coherent historical pair on the original inodes must still be refused.
	require.NoError(t, os.WriteFile(path, approvedDB, 0600))
	require.NoError(t, os.WriteFile(path+".checkpoint", approvedCheckpoint, 0600))
	rejected, err := open()
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.Nil(t, rejected)
	require.Equal(t, approvedDB, read(path))
	require.Equal(t, approvedCheckpoint, read(path+".checkpoint"))
	require.Equal(t, settled, read(w.path))

	// Even a matching digest cannot authorize a replacement inode, a missing
	// witness, or a registration on either of the two local devices.
	require.NoError(t, os.Rename(w.path, w.path+".original"))
	for _, present := range []bool{false, true} {
		if present {
			require.NoError(t, os.WriteFile(w.path, approvedCheckpoint, 0600))
		}
		rejected, err = open()
		require.ErrorIs(t, err, ErrDSGExactPreserve)
		require.Nil(t, rejected)
	}
	require.NoError(t, os.Rename(w.path+".original", w.path))
	require.NoError(t, os.WriteFile(w.path, approvedCheckpoint, 0600))
	require.False(t, w.matches(approvedCheckpoint, w.identity.Device))
	originalWorker := w.workerDevice
	w.workerDevice = w.identity.Device
	require.False(t, w.matches(approvedCheckpoint, ledgerDevice))
	w.workerDevice = originalWorker

	// A witness fault between the database commit and the final sync must
	// preserve the discrepancy across reopening, without repairing either file.
	authority, err = open()
	require.NoError(t, err)
	err = authority.update(func(tx *bolt.Tx) error {
		if err := os.Truncate(w.path, 8); err != nil {
			return err
		}
		_, err := tx.CreateBucket([]byte("synthetic-transition"))
		return err
	})
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.False(t, authority.verified())
	require.Len(t, read(w.path), 8)
	require.NotEqual(t, approvedCheckpoint, read(path+".checkpoint"))
	require.NoError(t, authority.db.Close())
	authority = nil
	rejected, err = open()
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.Nil(t, rejected)
}

func TestDSGLedgerGenerationReplay(t *testing.T) {
	stage := t.TempDir()
	require.NoError(t, os.Chmod(stage, 0700))
	a, err := dsgCreateSameMountAuthority(filepath.Join(stage, "grants.db"))
	require.NoError(t, err)
	defer a.db.Close()
	grant := dsgTestLedgerGrant(a.device + 1)
	require.NoError(t, a.approve(t.Context(), grant))
	for _, fault := range []string{"boot", "keeper", "enrollment", "mount-namespace"} {
		t.Run(fault, func(t *testing.T) {
			other := grant
			switch fault {
			case "boot":
				other.Evidence.Identity.BootSHA256 = dsgRecoveryHash([]byte("fresh-boot"))
			case "keeper":
				other.CustodySHA256 = dsgRecoveryHash([]byte("fresh-keeper"))
			case "enrollment":
				other.Evidence.Identity.EnrollmentSHA256 = dsgRecoveryHash([]byte("fresh-enrollment"))
			case "mount-namespace":
				other.Evidence.Identity.MountNS++
			}
			other.Evidence.Generation = other.Evidence.Identity.generation()
			other.Request.Generation = other.Evidence.Generation
			require.True(t, other.valid())
			require.ErrorIs(t, a.admit(t.Context(), other), ErrDSGExactPreserve)
			require.ErrorIs(t, a.advance(t.Context(), other, nil), ErrDSGExactPreserve)
			require.NoError(t, a.admit(t.Context(), grant))
		})
	}
}
