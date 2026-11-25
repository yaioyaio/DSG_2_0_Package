//go:build linux

package cache

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/core/content"

	ctdmeta "github.com/containerd/containerd/v2/core/metadata"
	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/containerd/errdefs"
	"github.com/moby/buildkit/cache/metadata"
	"github.com/moby/buildkit/identity"
	"github.com/moby/buildkit/solver"
	"github.com/moby/buildkit/solver/bboltcachestorage"
	"github.com/opencontainers/go-digest"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

// These writers create a native schema using only generated data. The recovered
// operation accepts an inherited fd for this private disposable ext4 fixture.
func dsgSeedReclaim(t *testing.T) (*os.File, dsgReclaimInput) {
	t.Helper()
	return dsgSeedReclaimAt(t, filepath.Join(t.TempDir(), dsgDeletionRoot))
}

// A fresh registered target is created only inside the caller-enrolled private mount.
func dsgSeedReclaimAt(t *testing.T, root string) (*os.File, dsgReclaimInput) {
	t.Helper()
	require.Equal(t, dsgDeletionRoot, filepath.Base(root))
	require.NoError(t, os.Mkdir(root, 0700))
	dir, err := os.Open(root)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, dir.Close()) })
	now := time.Now()
	p := dsgRecoveryPlan{Mode: dsgNativeReclamationQualification, Request: DSGExactIntent{
		Project: dsgRecoveryHash([]byte(dsgNativeReclamationQualification)), Worker: identity.NewID(), RecordID: identity.NewID(),
		Generation: dsgRecoveryHash([]byte(identity.NewID())), Origin: dsgRecoveryHash([]byte("test-owned native record")),
		Approval: dsgRecoveryHash([]byte(identity.NewID())), Intent: dsgRecoveryHash([]byte(identity.NewID())),
		ApprovedAtMS: now.Add(-time.Second).UnixMilli(), ExpiresAtMS: now.Add(2 * time.Minute).UnixMilli(),
	}}
	s := &dsgReclaimFixture{dsgDeletionFixture: &dsgDeletionFixture{dir: dir, root: root, plan: p}}
	ctx := namespaces.WithNamespace(t.Context(), dsgGraphNamespace)
	s.seedStores(t, ctx)
	journal, err := bolt.Open(filepath.Join(root, dsgDeletionJournal), 0600, nil)
	require.NoError(t, err)
	require.NoError(t, journal.Close())
	history, err := bolt.Open(filepath.Join(root, reclaimHistory), 0600, nil)
	require.NoError(t, err)
	require.NoError(t, history.Update(func(tx *bolt.Tx) error {
		return dsgGraphTestBucket(t, tx, "_version").Put([]byte("version"), []byte("2"))
	}))
	require.NoError(t, history.View(func(tx *bolt.Tx) error {
		s.shape.History, err = dsgTrialTree(tx, func([]string) bool { return false })
		return err
	}))
	require.NoError(t, history.Close())
	graph, err := s.graph(t.Context())
	require.NoError(t, err)
	require.NotNil(t, graph.Ownership)
	require.Equal(t, "exact_record_resources", graph.Ownership.Reason, "%v", graph.Unverified)
	require.True(t, graph.Ownership.Exclusive && graph.Ownership.RecordLease && graph.Ownership.VariantLease)
	require.Equal(t, 1, graph.Ownership.OwnedChildSnapshots)
	require.Equal(t, 1, graph.Solver.ResultNodes)
	s.shape.Graph = graph.EvidenceHash
	s.layout.Identities, err = s.identities()
	require.NoError(t, err)
	s.shape.Backing = s.layout
	s.shape.Files, err = s.fileIDs()
	require.NoError(t, err)
	s.plan.GraphSHA256 = dsgDeletionHash(s.shape)
	for n := range s.plan.stages() {
		target, peer, err := s.evidence(ctx, n)
		require.NoError(t, err)
		require.Len(t, target, 64)
		require.Len(t, peer, 64)
		obj := &s.plan.Objects[s.plan.participantIndex(n)]
		if n == 2 {
			obj = &s.plan.View
		}
		obj.BeforeSHA256, obj.NonTargetSHA256 = target, peer
	}
	s.plan.Request.MetadataDigest = s.plan.Objects[4].BeforeSHA256
	require.True(t, s.plan.valid(dsgNativeReclamationQualification))
	return dir, dsgReclaimInput{s.plan, s.shape}
}

func (s *dsgReclaimFixture) seedStores(t *testing.T, ctx context.Context) {
	root := s.root
	sn, err := native.NewSnapshotter(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	// The pinned cache manager's test constructor puts its DBs and local content
	// in one directory. Move only its closed native stores on the same filesystem.
	staging := t.TempDir()
	bootstrap, err := os.OpenFile(filepath.Join(staging, "containerdmeta.db"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	require.NoError(t, err)
	require.NoError(t, bootstrap.Close())
	co, closeWorker, err := newCacheManager(ctx, t, cmOpt{tmpdir: staging, snapshotter: sn, snapshotterName: "native"})
	require.NoError(t, err)
	solverStore, err := bboltcachestorage.NewStore(filepath.Join(root, reclaimSolver))
	require.NoError(t, err)
	for i, role := range []string{"target", "peer"} {
		payload := []byte(s.payload("snapshot", role))
		rec := dsgCreateManifestlessRecord(t, ctx, co, payload)
		require.Equal(t, filepath.Join(root, "snapshots", "snapshots", rec.AllocationID), rec.AllocationRoot)
		require.Equal(t, rec.Layer.Digest, digest.FromBytes(rec.LayerBytes))
		s.shape.Blobs[i] = dsgReclaimBlob{rec.Layer.Digest, rec.Layer.Size}
		origin := digest.FromBytes(append([]byte("manifestless-native-payload-v1\x00"), payload...))
		require.NoError(t, solverStore.AddResult(origin.String(), solver.CacheResult{ID: s.plan.Request.Worker + "::" + rec.RecordID, CreatedAt: time.Now().UTC()}))
		if role == "target" {
			s.shape.Snapshot = rec.SnapshotID
			s.layout.TargetID = rec.AllocationID
			s.plan.Request.RecordID = rec.RecordID
			s.plan.Request.Origin = origin.Encoded()
			s.plan.Objects[0].Key = origin.String()
			s.plan.Objects[1].Key = rec.RecordID
			s.plan.View.Key = digest.FromString("native view/" + rec.SnapshotID + "-view").String()
			s.plan.Objects[2].Key = rec.SnapshotID
			s.plan.Objects[3].Key = rec.Layer.Digest.String()
			s.plan.Objects[4].Key = rec.RecordID
		} else {
			s.layout.PeerID = rec.AllocationID
			s.layout.PeerKey = rec.SnapshotID
			s.shape.PeerRecord = rec.RecordID
			s.shape.PeerNode = origin.String()
		}
	}
	require.NoError(t, solverStore.Close())
	closeWorker()
	require.NoError(t, sn.Close())
	require.NoError(t, os.Mkdir(filepath.Join(root, "content"), 0700))
	for _, pair := range [][2]string{{"metadata.db", reclaimCache}, {"containerdmeta.db", reclaimBackend}, {"blobs", "content/blobs"}, {"ingest", "content/ingest"}} {
		require.NoError(t, os.Rename(filepath.Join(staging, pair[0]), filepath.Join(root, pair[1])))
	}
	require.NoError(t, s.withDB(reclaimBackend, true, func(tx *bolt.Tx) error {
		table := dsgGraphNativeBucket(tx.Bucket([]byte("v1")).Bucket([]byte(dsgGraphNamespace)))
		target, view := table.Bucket([]byte(s.shape.Snapshot)), table.Bucket([]byte(s.shape.Snapshot+"-view"))
		if target == nil || view == nil {
			return ErrDSGExactPreserve
		}
		s.shape.Native = string(target.Get([]byte("name")))
		s.shape.NativeView = string(view.Get([]byte("name")))
		return nil
	}))
	cacheStore, err := metadata.NewStore(filepath.Join(root, reclaimCache))
	require.NoError(t, err)
	target, ok := cacheStore.Get(s.plan.Request.RecordID)
	require.True(t, ok)
	indexes := target.Indexes()
	slices.Sort(indexes)
	require.NotEmpty(t, indexes)
	require.LessOrEqual(t, len(indexes), 8)
	s.shape.CacheIndexes = indexes
	require.NoError(t, cacheStore.DB().View(func(tx *bolt.Tx) error {
		index := tx.Bucket([]byte("_index"))
		if index == nil {
			return ErrDSGExactPreserve
		}
		var reverse []string
		suffix := "::" + s.plan.Request.RecordID
		if err := index.ForEach(func(k, v []byte) error {
			if strings.HasSuffix(string(k), suffix) {
				if v == nil || len(v) != 0 {
					return ErrDSGExactPreserve
				}
				reverse = append(reverse, strings.TrimSuffix(string(k), suffix))
			}
			return nil
		}); err != nil {
			return err
		}
		if !slices.Equal(indexes, reverse) {
			return ErrDSGExactPreserve
		}
		ext := tx.Bucket([]byte("_external"))
		if ext != nil && ext.Bucket([]byte(s.plan.Request.RecordID)) != nil {
			return ErrDSGExactPreserve
		}
		return nil
	}))
	require.NoError(t, cacheStore.Close())
	require.NoError(t, s.withDB(reclaimNative, true, func(tx *bolt.Tx) error {
		view, err := dsgGraphReadNativeSnapshot(tx.Bucket([]byte("v1")).Bucket([]byte("snapshots")).Bucket([]byte(s.shape.NativeView)))
		if err != nil {
			return err
		}
		if view.kind != snapshots.KindView || view.parent != s.shape.Native {
			return ErrDSGExactPreserve
		}
		s.shape.ViewID = view.id
		return nil
	}))
}

// Reopen the participating stores through their public reader interfaces. The
// durable final status is checked again after these independent observations.
func (s *dsgReclaimFixture) verifyConsumers(t *testing.T) {
	t.Helper()
	ctx := namespaces.WithNamespace(t.Context(), dsgGraphNamespace)
	solverStore, err := bboltcachestorage.NewStore(filepath.Join(s.root, reclaimSolver))
	require.NoError(t, err)
	defer solverStore.Close()
	worker := s.plan.Request.Worker + "::"
	target, peer := s.plan.Request.RecordID, s.shape.PeerRecord
	_, err = solverStore.Load(s.plan.Objects[0].Key, worker+target)
	require.ErrorIs(t, err, solver.ErrNotFound)
	var reverse []string
	require.NoError(t, solverStore.WalkIDsByResult(worker+target, func(node string) error { reverse = append(reverse, node); return nil }))
	require.Empty(t, reverse)
	peerNode := s.shape.PeerNode
	res, err := solverStore.Load(peerNode, worker+peer)
	require.NoError(t, err)
	require.Equal(t, worker+peer, res.ID)
	md, err := metadata.NewStore(filepath.Join(s.root, reclaimCache))
	require.NoError(t, err)
	defer md.Close()
	items, err := md.All()
	require.NoError(t, err)
	require.Len(t, items, 1)
	require.Equal(t, peer, items[0].ID())
	cs, err := local.NewStore(filepath.Join(s.root, "content"))
	require.NoError(t, err)
	sn, err := native.NewSnapshotter(filepath.Join(s.root, "snapshots"))
	require.NoError(t, err)
	defer sn.Close()
	backend, err := bolt.Open(filepath.Join(s.root, reclaimBackend), 0600, &bolt.Options{ReadOnly: true, Timeout: time.Second})
	require.NoError(t, err)
	defer backend.Close()
	mdb := ctdmeta.NewDB(backend, cs, map[string]snapshots.Snapshotter{"native": sn})
	list, err := ctdmeta.NewLeaseManager(mdb).List(ctx)
	require.NoError(t, err)
	var leaseIDs []string
	for _, l := range list {
		leaseIDs = append(leaseIDs, l.ID)
	}
	require.ElementsMatch(t, []string{peer, peer + "-variants"}, leaseIDs)
	for _, key := range []string{s.shape.Snapshot, s.shape.Snapshot + "-view"} {
		_, err := mdb.Snapshotter("native").Stat(ctx, key)
		require.True(t, errdefs.IsNotFound(err))
	}
	_, err = mdb.Snapshotter("native").Stat(ctx, s.layout.PeerKey)
	require.NoError(t, err)
	for _, key := range []string{s.shape.Native, s.shape.NativeView} {
		_, err := sn.Stat(ctx, key)
		require.True(t, errdefs.IsNotFound(err))
	}
	for _, store := range []content.Store{mdb.ContentStore(), cs} {
		_, err := store.Info(ctx, s.blob("target").Digest)
		require.True(t, errdefs.IsNotFound(err))
		_, err = store.Info(ctx, s.blob("peer").Digest)
		require.NoError(t, err)
	}
}
func dsgOpenReclaim(t *testing.T, dir *os.File, in dsgReclaimInput) (*dsgReclaimFixture, *dsgRecoveryJournal) {
	t.Helper()
	root, err := os.Readlink(fmt.Sprintf("/proc/self/fd/%d", dir.Fd()))
	require.NoError(t, err)
	require.True(t, in.Plan.valid(dsgNativeReclamationQualification))
	require.Equal(t, in.Plan.GraphSHA256, dsgDeletionHash(in.Layout))
	require.Equal(t, in.Layout.Snapshot, in.Plan.Objects[2].Key)
	require.Equal(t, in.Plan.Request.RecordID, in.Plan.Objects[1].Key)
	require.Equal(t, digest.FromString("native view/"+in.Layout.Snapshot+"-view").String(), in.Plan.View.Key)
	s := &dsgReclaimFixture{&dsgDeletionFixture{dir: dir, root: root, plan: in.Plan, layout: in.Layout.Backing}, in.Layout}
	require.Equal(t, s.blob("target").Digest.String(), in.Plan.Objects[3].Key)
	source := digest.FromBytes(append([]byte("manifestless-native-payload-v1\x00"), []byte(s.payload("snapshot", "target"))...))
	require.Equal(t, source.Encoded(), in.Plan.Request.Origin)
	require.Equal(t, source.String(), in.Plan.Objects[0].Key)
	require.NoError(t, s.guard())
	s.db, err = bolt.Open(filepath.Join(root, dsgDeletionJournal), 0600, &bolt.Options{Timeout: time.Second})
	require.NoError(t, err)
	j := &dsgRecoveryJournal{root: root, db: s.db, qualificationMode: in.Plan.Mode}
	for n := range j.store {
		j.store[n] = s
	}
	return s, j
}

func dsgReclaimChild(t *testing.T, domain, barrier string) {
	dir := os.NewFile(3, "inherited-private-record-fixture")
	require.NotNil(t, dir)
	var in dsgReclaimInput
	require.NoError(t, json.NewDecoder(io.LimitReader(os.Stdin, 8192)).Decode(&in))
	s, j := dsgOpenReclaim(t, dir, in)
	n := slices.Index(in.Plan.stages(), domain)
	require.NotEqual(t, -1, n)
	state, err := j.status(t.Context(), in.Plan)
	require.NoError(t, err)
	require.Equal(t, n, state.Confirmed)
	err = j.advance(t.Context(), in.Plan, func(point string) {
		if point == barrier && point == "armed" && s.calls == 0 {
			os.Exit(85)
		}
		if point == barrier && point == "effect" && s.calls == 1 {
			os.Exit(86)
		}
	})
	t.Fatalf("fixture effect did not reach crash boundary: %v", err)
}

func dsgRunReclaimChild(t *testing.T, dir *os.File, in dsgReclaimInput, domain, barrier string, code int) {
	t.Helper()
	raw, err := json.Marshal(in)
	require.NoError(t, err)
	exe, err := os.Executable()
	require.NoError(t, err)
	ctx, cancel := context.WithTimeout(t.Context(), 8*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, exe, "-test.run=^TestDSGNativeRecordReclamation$", "--", "dsg-record-fixture-child", domain, barrier)
	cmd.Env = []string{"TMPDIR=" + filepath.Dir(dir.Name())}
	cmd.ExtraFiles = []*os.File{dir}
	cmd.Stdin = bytes.NewReader(raw)
	out, err := cmd.CombinedOutput()
	require.NotNil(t, cmd.ProcessState)
	if cmd.ProcessState.ExitCode() != code {
		lines := regexp.MustCompile(`dsg_native_reclamation[a-z_]*_test\.go:[0-9]+`).FindAll(out, 6)
		t.Fatalf("fixture child failed: %v (%s)", err, lines)
	}
}

func TestDSGNativeRecordReclamation(t *testing.T) {
	args := os.Args
	if len(args) > 3 && args[len(args)-3] == "dsg-record-fixture-child" {
		dsgReclaimChild(t, args[len(args)-2], args[len(args)-1])
		return
	}
	dir, in := dsgSeedReclaim(t)
	ctx, p := t.Context(), in.Plan
	s, j := dsgOpenReclaim(t, dir, in)
	require.NoError(t, j.begin(ctx, p))
	require.NoError(t, s.db.Close())
	for n, domain := range p.stages() {
		if !t.Run(domain, func(t *testing.T) {
			dsgRunReclaimChild(t, dir, in, domain, "armed", 85)
			s, j := dsgOpenReclaim(t, dir, in)
			armed, err := j.status(ctx, p)
			require.NoError(t, err)
			require.True(t, armed.Armed)
			require.Equal(t, n, armed.Confirmed)
			marked, err := s.callMarker(p, n)
			require.NoError(t, err)
			require.False(t, marked)
			require.NoError(t, s.db.Close())
			dsgRunReclaimChild(t, dir, in, domain, "effect", 86)
			s, j = dsgOpenReclaim(t, dir, in)
			defer func() { require.NoError(t, s.db.Close()) }()
			pending, err := j.status(ctx, p)
			require.NoError(t, err)
			require.Equal(t, armed, pending)
			after, err := s.observe(ctx, p, n)
			require.NoError(t, err)
			require.Equal(t, dsgRecoveryAfter, after)
			var points []string
			require.NoError(t, j.advance(ctx, p, func(point string) { points = append(points, point) }))
			require.Equal(t, []string{"ack"}, points)
			require.Zero(t, s.calls)
		}) {
			t.FailNow()
		}
	}
	s, j = dsgOpenReclaim(t, dir, in)
	s.verifyConsumers(t)
	completed, err := j.status(ctx, p)
	require.NoError(t, err)
	require.Equal(t, len(p.stages()), completed.Confirmed)
	require.Equal(t, p.proof(len(p.stages())-1), completed.TrialProof)
	require.Nil(t, completed.Receipt)
	require.ErrorIs(t, j.advance(ctx, p, nil), ErrDSGExactPreserve)
	require.ErrorIs(t, j.begin(ctx, p), ErrDSGExactPreserve)
	require.NoError(t, s.db.View(func(tx *bolt.Tx) error {
		require.Equal(t, len(p.stages()), tx.Bucket([]byte(dsgDeletionCalls)).Stats().KeyN)
		return nil
	}))
	require.Zero(t, s.calls)
	require.NoError(t, s.db.Close())
	t.Log(dsgNativeReclamationQualification + " stages=6 armed_crashes=6 effect_crashes=6 replay_calls=0 non_target_changes=0 logical_records=1 production_receipts=0 generated_by=cache_manager_getremotes source_bound=1")
}
