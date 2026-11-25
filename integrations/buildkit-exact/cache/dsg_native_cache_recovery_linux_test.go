//go:build linux

package cache

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/moby/buildkit/cache/metadata"
	"github.com/moby/buildkit/identity"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

const dsgTrialRetainProof = "dsg.trial.cache-retain-proof.v1"

type dsgCacheTrialGrant struct {
	Target, Peer, Intent, OtherFields string
	ApprovedAtMS, ExpiresAtMS         int64
}

// Cover every native record, index and external metadata field, apart from
// exactly the two fields the retain operation owns in its target record.
func (g dsgCacheTrialGrant) peers(tx *bolt.Tx) (string, error) {
	return dsgTrialTree(tx, func(path []string) bool {
		return slices.Equal(path, []string{dsgMain, g.Target, keyCachePolicy}) ||
			slices.Equal(path, []string{dsgMain, g.Target, dsgTrialRetainProof})
	})
}

func dsgCacheTrialValue(v any) []byte {
	// Callers supply only fixed enum values or a hexadecimal digest.
	mv, _ := metadata.NewValue(v)
	raw, _ := json.Marshal(mv)
	return raw
}

func (g dsgCacheTrialGrant) proof() string {
	raw, _ := json.Marshal(g)
	return dsgRecoveryHash(append([]byte("native-cache-retain-v1/"), raw...))
}

func (g dsgCacheTrialGrant) state(tx *bolt.Tx) (dsgRecoveryObservation, error) {
	peers, err := g.peers(tx)
	if err != nil || !dsgFullID.MatchString(g.Target) || !dsgFullID.MatchString(g.Peer) || g.Target == g.Peer || !dsgDigest.MatchString(g.Intent) || peers != g.OtherFields {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	main := tx.Bucket([]byte(dsgMain))
	if main == nil || main.Bucket([]byte(g.Peer)) == nil {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	target := main.Bucket([]byte(g.Target))
	if target == nil || target.Bucket([]byte(dsgTrialRetainProof)) != nil {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	policy, proof := target.Get([]byte(keyCachePolicy)), target.Get([]byte(dsgTrialRetainProof))
	switch {
	case bytes.Equal(policy, dsgCacheTrialValue(cachePolicyDefault)) && proof == nil:
		return dsgRecoveryBefore, nil
	case bytes.Equal(policy, dsgCacheTrialValue(cachePolicyRetain)) && bytes.Equal(proof, dsgCacheTrialValue(g.proof())):
		return dsgRecoveryAfter, nil
	default:
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
}

func (g dsgCacheTrialGrant) retain(ctx context.Context, store *metadata.Store) error {
	item, ok := store.Get(g.Target)
	if !ok {
		return ErrDSGExactPreserve
	}
	return store.DB().Update(func(tx *bolt.Tx) error {
		before, err := g.state(tx)
		now := time.Now().UnixMilli()
		if before != dsgRecoveryBefore || err != nil || ctx.Err() != nil || now < g.ApprovedAtMS || now >= g.ExpiresAtMS {
			return ErrDSGExactPreserve
		}
		b := tx.Bucket([]byte(dsgMain)).Bucket([]byte(g.Target))
		retain, err := metadata.NewValue(cachePolicyRetain)
		if err != nil {
			return err
		}
		witness, err := metadata.NewValue(g.proof())
		if err != nil {
			return err
		}
		if err := item.SetValue(b, keyCachePolicy, retain); err != nil {
			return err
		}
		return item.SetValue(b, dsgTrialRetainProof, witness)
	})
}

func dsgRunNativeCacheTrialChild(t *testing.T) {
	t.Helper()
	parent, err := os.Readlink("/proc/self/fd/3")
	require.NoError(t, err)
	root := filepath.Join(parent, "fresh-native-cache")
	require.NoError(t, os.Mkdir(root, 0700))
	ctx := namespaces.WithNamespace(t.Context(), dsgGraphNamespace)
	sn, err := native.NewSnapshotter(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	worker, closeWorker, err := newCacheManager(ctx, t, cmOpt{tmpdir: root, snapshotter: sn, snapshotterName: "native"})
	require.NoError(t, err)
	target, err := worker.manager.New(ctx, nil, nil, CachePolicyDefault)
	require.NoError(t, err)
	peer, err := worker.manager.New(ctx, nil, nil, CachePolicyDefault)
	require.NoError(t, err)
	now := time.Now()
	g := dsgCacheTrialGrant{Target: target.ID(), Peer: peer.ID(), Intent: dsgRecoveryHash([]byte(identity.NewID())), ApprovedAtMS: now.UnixMilli(), ExpiresAtMS: now.Add(time.Minute).UnixMilli()}
	closeWorker()
	require.NoError(t, sn.Close())
	store, err := metadata.NewStore(filepath.Join(root, "metadata.db"))
	require.NoError(t, err)
	require.NoError(t, store.DB().View(func(tx *bolt.Tx) error { g.OtherFields, err = g.peers(tx); return err }))
	raw, err := json.Marshal(g)
	require.NoError(t, err)
	f, err := os.OpenFile(filepath.Join(root, "trial-grant.json"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	require.NoError(t, err)
	_, err = f.Write(raw)
	require.NoError(t, err)
	require.NoError(t, f.Sync())
	require.NoError(t, f.Close())
	require.NoError(t, DSGWithDedicatedMaintenance(root, func() error {
		if err := g.retain(ctx, store); err != nil {
			return err
		}
		os.Exit(87) // commit returned; store and fence are still open
		return nil
	}))
}

func TestDSGNativeCacheMetadataRecovery(t *testing.T) {
	args := os.Args
	if args[len(args)-1] == "dsg-new-native-cache-trial" {
		dsgRunNativeCacheTrialChild(t)
		return
	}
	parent := t.TempDir()
	dir, err := os.Open(parent)
	require.NoError(t, err)
	defer dir.Close()
	exe, err := os.Executable()
	require.NoError(t, err)
	ctx, cancel := context.WithTimeout(t.Context(), 25*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, exe, "-test.run=^TestDSGNativeCacheMetadataRecovery$", "--", "dsg-new-native-cache-trial")
	cmd.Env = []string{"TMPDIR=" + parent}
	cmd.ExtraFiles = []*os.File{dir}
	output, err := cmd.CombinedOutput()
	require.NotNil(t, cmd.ProcessState)
	if cmd.ProcessState.ExitCode() != 87 {
		lines := regexp.MustCompile(`dsg_native_cache_recovery_linux_test\.go:[0-9]+`).FindAll(output, 6)
		t.Fatalf("cache trial child failed: %v (%s)", err, lines)
	}
	root := filepath.Join(parent, "fresh-native-cache")
	raw, err := os.ReadFile(filepath.Join(root, "trial-grant.json"))
	require.NoError(t, err)
	var g dsgCacheTrialGrant
	require.NoError(t, json.Unmarshal(raw, &g))
	require.NoError(t, DSGWithDedicatedMaintenance(root, func() error {
		store, err := metadata.NewStore(filepath.Join(root, "metadata.db"))
		require.NoError(t, err)
		defer func() { require.NoError(t, store.Close()) }()
		var previousTx int
		require.NoError(t, store.DB().View(func(tx *bolt.Tx) error {
			state, err := g.state(tx)
			require.NoError(t, err)
			require.Equal(t, dsgRecoveryAfter, state)
			other := g
			other.Intent = dsgRecoveryHash([]byte(identity.NewID()))
			_, err = other.state(tx)
			require.ErrorIs(t, err, ErrDSGExactPreserve)
			previousTx = tx.ID()
			return nil
		}))
		target, ok := store.Get(g.Target)
		require.True(t, ok)
		peer, ok := store.Get(g.Peer)
		require.True(t, ok)
		require.True(t, (&cacheMetadata{si: target}).HasCachePolicyRetain())
		require.True(t, (&cacheMetadata{si: peer}).HasCachePolicyDefault())
		require.ErrorIs(t, g.retain(t.Context(), store), ErrDSGExactPreserve)
		return store.DB().View(func(tx *bolt.Tx) error {
			require.Equal(t, previousTx, tx.ID())
			state, err := g.state(tx)
			require.Equal(t, dsgRecoveryAfter, state)
			return err
		})
	}))
}
