//go:build linux

package cache

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
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
