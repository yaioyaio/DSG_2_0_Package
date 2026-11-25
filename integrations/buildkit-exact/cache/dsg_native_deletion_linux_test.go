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
	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/containerd/errdefs"
	"github.com/moby/buildkit/identity"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

func (s *dsgDeletionFixture) evidence(ctx context.Context, stage int) (target, peer string, err error) {
	identities, err := s.identities()
	if err != nil || identities != s.layout.Identities {
		return "", "", ErrDSGExactPreserve
	}
	if _, err := s.names(".", []string{"snapshots", "content", dsgDeletionJournal}, ""); err != nil {
		return "", "", err
	}
	if _, err := s.names("snapshots", []string{"snapshots", "metadata.db"}, ""); err != nil {
		return "", "", err
	}
	switch stage {
	case 0:
		return s.snapshot(ctx)
	case 1:
		return s.content(ctx)
	default:
		return "", "", ErrDSGExactPreserve
	}
}

func (s *dsgDeletionFixture) observe(ctx context.Context, p dsgRecoveryPlan, stage int) (dsgRecoveryObservation, error) {
	if p != s.plan || !p.valid(dsgNativeDeletionQualification) || stage < 0 || stage >= len(p.stages()) {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	target, peer, err := s.evidence(ctx, stage)
	want := p.Objects[stage]
	if err != nil || peer != want.NonTargetSHA256 {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	marked, err := s.callMarker(p, stage)
	if err != nil {
		return dsgRecoveryUnknown, err
	}
	switch {
	case !marked && target == want.BeforeSHA256:
		return dsgRecoveryBefore, nil
	case marked && target == "":
		// Only this test's fenced fixture admits this inference. A valid
		// authorization, an attempt marker or absence alone proves nothing.
		return dsgRecoveryAfter, nil
	default:
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
}

func (s *dsgDeletionFixture) commit(ctx context.Context, p dsgRecoveryPlan, stage int) error {
	state, err := s.observe(ctx, p, stage)
	if err != nil || state != dsgRecoveryBefore {
		return ErrDSGExactPreserve
	}
	// The journal holds the exclusive fixture boundary across preimage,
	// durable invocation marker, native effect and postcondition.
	var remove func(context.Context) error
	var parent string
	switch stage {
	case 0:
		sn, err := native.NewSnapshotter(filepath.Join(s.root, "snapshots"))
		if err != nil {
			return err
		}
		defer sn.Close()
		remove = func(ctx context.Context) error { return sn.Remove(ctx, p.Objects[stage].Key) }
		parent = "snapshots/snapshots"
	case 1:
		cs, err := local.NewStore(filepath.Join(s.root, "content"))
		if err != nil {
			return err
		}
		remove = func(ctx context.Context) error { return cs.Delete(ctx, s.blob("target").Digest) }
		parent = "content/blobs/sha256"
	default:
		return ErrDSGExactPreserve
	}
	if err := s.recordCall(ctx, p, stage); err != nil {
		return err
	}
	if !dsgRecoveryAuthorized(ctx, p) {
		return ErrDSGExactPreserve
	}
	s.calls++
	if err := remove(ctx); err != nil {
		return err
	}
	dir, err := os.Open(filepath.Join(s.root, parent))
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}

type dsgDeletionChildInput struct {
	Plan   dsgRecoveryPlan
	Layout dsgDeletionLayout
}

func dsgOpenDeletionFixture(t *testing.T, dir *os.File, input dsgDeletionChildInput) (*dsgDeletionFixture, *dsgRecoveryJournal) {
	t.Helper()
	root, err := os.Readlink("/proc/self/fd/" + fmt.Sprint(dir.Fd()))
	require.NoError(t, err)
	s := &dsgDeletionFixture{dir: dir, root: root, plan: input.Plan, layout: input.Layout}
	require.True(t, s.plan.valid(dsgNativeDeletionQualification))
	require.Equal(t, s.plan.GraphSHA256, dsgDeletionHash(s.layout))
	identities, err := s.identities()
	require.NoError(t, err)
	require.Equal(t, s.layout.Identities, identities)
	s.db, err = bolt.Open(filepath.Join(root, dsgDeletionJournal), 0600, &bolt.Options{Timeout: time.Second})
	require.NoError(t, err)
	j := &dsgRecoveryJournal{db: s.db, root: s.root, qualificationMode: dsgNativeDeletionQualification}
	for n := range s.plan.stages() {
		j.store[n] = s
	}
	return s, j
}

func dsgSeedDeletionFixture(t *testing.T) (*os.File, dsgDeletionChildInput) {
	t.Helper()
	ctx := t.Context()
	root := filepath.Join(t.TempDir(), dsgDeletionRoot)
	require.NoError(t, os.Mkdir(root, 0700))
	dir, err := os.Open(root)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, dir.Close()) })
	var fs unix.Statfs_t
	require.NoError(t, unix.Fstatfs(int(dir.Fd()), &fs))
	require.Equal(t, int64(unix.EXT4_SUPER_MAGIC), int64(fs.Type))
	require.Zero(t, fs.Flags&unix.ST_RDONLY)
	now := time.Now()
	s := &dsgDeletionFixture{dir: dir, root: root, plan: dsgRecoveryPlan{Mode: dsgNativeDeletionQualification, Request: DSGExactIntent{
		Project: dsgRecoveryHash([]byte(dsgNativeDeletionQualification)), Worker: identity.NewID(), RecordID: identity.NewID(),
		Generation: dsgRecoveryHash([]byte(identity.NewID())), Origin: dsgRecoveryHash([]byte("test-owned native stores")),
		Approval: dsgRecoveryHash([]byte(identity.NewID())), Intent: dsgRecoveryHash([]byte(identity.NewID())),
		ApprovedAtMS: now.Add(-time.Second).UnixMilli(), ExpiresAtMS: now.Add(2 * time.Minute).UnixMilli(),
	}}}
	sn, err := native.NewSnapshotter(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	s.layout.PeerKey = identity.NewID()
	seedSnapshot := func(role, key string) string {
		active := "fixture-active-" + role
		id := dsgBackingPrepare(t, ctx, sn, root, active, "")
		dsgBackingWrite(t, dsgBackingSnapshotPath(root, id, "payload"), []byte(s.payload("snapshot", role)))
		require.NoError(t, sn.Commit(ctx, key, active, snapshots.WithLabels(map[string]string{dsgDeletionFixtureLabel: s.plan.Request.Generation})))
		return id
	}
	s.layout.TargetID = seedSnapshot("target", s.plan.Request.RecordID)
	s.layout.PeerID = seedSnapshot("peer", s.layout.PeerKey)
	require.NoError(t, sn.Close())
	cs, err := local.NewStore(filepath.Join(root, "content"))
	require.NoError(t, err)
	for _, role := range []string{"target", "peer"} {
		require.NoError(t, content.WriteBlob(ctx, cs, "fixture-"+role, strings.NewReader(s.payload("blob", role)), s.blob(role)))
	}
	s.plan.Objects[0].Key = s.plan.Request.RecordID
	s.plan.Objects[1].Key = s.blob("target").Digest.String()
	db, err := bolt.Open(filepath.Join(root, dsgDeletionJournal), 0600, nil)
	require.NoError(t, err)
	require.NoError(t, db.Close())
	s.layout.Identities, err = s.identities()
	require.NoError(t, err)
	s.plan.GraphSHA256 = dsgDeletionHash(s.layout)
	for n := range s.plan.stages() {
		target, peer, err := s.evidence(ctx, n)
		require.NoError(t, err)
		require.Len(t, target, 64)
		require.Len(t, peer, 64)
		s.plan.Objects[n].BeforeSHA256, s.plan.Objects[n].NonTargetSHA256 = target, peer
	}
	s.plan.Request.MetadataDigest = s.plan.Objects[0].BeforeSHA256
	return dir, dsgDeletionChildInput{Plan: s.plan, Layout: s.layout}
}

func dsgDeletionChild(t *testing.T, domain, barrier string) {
	dir := os.NewFile(3, "inherited-private-deletion-fixture")
	require.NotNil(t, dir)
	var input dsgDeletionChildInput
	require.NoError(t, json.NewDecoder(io.LimitReader(os.Stdin, 8192)).Decode(&input))
	s, j := dsgOpenDeletionFixture(t, dir, input)
	stage := slices.Index(s.plan.stages(), domain)
	require.NotEqual(t, -1, stage)
	state, err := j.status(t.Context(), s.plan)
	require.NoError(t, err)
	require.Equal(t, stage, state.Confirmed)
	err = j.advance(t.Context(), s.plan, func(point string) {
		if point != barrier {
			return
		}
		if point == "armed" && s.calls == 0 {
			os.Exit(85)
		}
		if point == "effect" && s.calls == 1 {
			os.Exit(86)
		}
	})
	t.Fatalf("expected fixture process exit at %s: %v", barrier, err)
}

func dsgRunDeletionChild(t *testing.T, dir *os.File, input dsgDeletionChildInput, domain, barrier string, code int) {
	t.Helper()
	executable, err := os.Executable()
	require.NoError(t, err)
	raw, err := json.Marshal(input)
	require.NoError(t, err)
	ctx, cancel := context.WithTimeout(t.Context(), 8*time.Second)
	defer cancel()
	child := exec.CommandContext(ctx, executable, "-test.run=^TestDSGNativeDeletionJournalRecovery$", "--", "native-deletion-child", domain, barrier)
	child.Env = []string{"TMPDIR=" + filepath.Dir(dir.Name())}
	child.ExtraFiles = []*os.File{dir}
	child.Stdin = bytes.NewReader(raw)
	out, err := child.CombinedOutput()
	require.NotNil(t, child.ProcessState)
	if child.ProcessState.ExitCode() != code {
		lines := regexp.MustCompile(`dsg_native_deletion_[a-z_]+_test\.go:[0-9]+`).FindAll(out, 6)
		t.Fatalf("native deletion child failed: %v (%s)", err, lines)
	}
}

func TestDSGNativeDeletionJournalRecovery(t *testing.T) {
	if args := os.Args; len(args) > 3 && args[len(args)-3] == "native-deletion-child" {
		dsgDeletionChild(t, args[len(args)-2], args[len(args)-1])
		return
	}
	dir, input := dsgSeedDeletionFixture(t)
	p := input.Plan
	s, j := dsgOpenDeletionFixture(t, dir, input)
	require.NoError(t, j.begin(t.Context(), p))
	require.NoError(t, s.db.Close())
	for stage, domain := range p.stages() {
		ok := t.Run(domain, func(t *testing.T) {
			ctx := t.Context()
			dsgRunDeletionChild(t, dir, input, domain, "armed", 85)
			s, j := dsgOpenDeletionFixture(t, dir, input)
			armed, err := j.status(ctx, p)
			require.NoError(t, err)
			require.True(t, armed.Armed)
			require.Equal(t, stage, armed.Confirmed)
			marked, err := s.callMarker(p, stage)
			require.NoError(t, err)
			require.False(t, marked)
			before, err := s.observe(ctx, p, stage)
			require.NoError(t, err)
			require.Equal(t, dsgRecoveryBefore, before)
			require.NoError(t, s.db.Close())

			dsgRunDeletionChild(t, dir, input, domain, "effect", 86)
			s, j = dsgOpenDeletionFixture(t, dir, input)
			defer func() { require.NoError(t, s.db.Close()) }()
			pending, err := j.status(ctx, p)
			require.NoError(t, err)
			require.Equal(t, armed, pending)
			after, err := s.observe(ctx, p, stage)
			require.NoError(t, err)
			require.Equal(t, dsgRecoveryAfter, after)
			if domain == "snapshot" {
				sn, err := native.NewSnapshotter(filepath.Join(s.root, "snapshots"))
				require.NoError(t, err)
				_, missing := sn.Stat(ctx, p.Objects[stage].Key)
				require.True(t, errdefs.IsNotFound(missing))
				_, err = sn.Stat(ctx, s.layout.PeerKey)
				require.NoError(t, err)
				require.NoError(t, sn.Close())
			} else {
				cs, err := local.NewStore(filepath.Join(s.root, "content"))
				require.NoError(t, err)
				_, missing := cs.Info(ctx, s.blob("target").Digest)
				require.True(t, errdefs.IsNotFound(missing))
				_, err = cs.Info(ctx, s.blob("peer").Digest)
				require.NoError(t, err)
			}
			var barriers []string
			require.NoError(t, j.advance(ctx, p, func(point string) { barriers = append(barriers, point) }))
			require.Equal(t, []string{"ack"}, barriers)
			require.Zero(t, s.calls)
			confirmed, err := j.status(ctx, p)
			require.NoError(t, err)
			require.Equal(t, stage+1, confirmed.Confirmed)
			require.False(t, confirmed.Armed)
			require.Nil(t, confirmed.Receipt)
			if stage+1 == len(p.stages()) {
				require.Equal(t, p.proof(stage), confirmed.TrialProof)
				fabricated := confirmed
				fabricated.TrialProof = ""
				fabricated.Receipt = &DSGExactReceipt{Request: p.Request, LogicalRecords: 1}
				require.False(t, fabricated.valid(p))
				require.ErrorIs(t, j.advance(ctx, p, nil), ErrDSGExactPreserve)
				require.ErrorIs(t, j.begin(ctx, p), ErrDSGExactPreserve)
				require.Zero(t, s.calls)
				require.NoError(t, s.db.View(func(tx *bolt.Tx) error {
					require.Equal(t, len(p.stages()), tx.Bucket([]byte(dsgDeletionCalls)).Stats().KeyN)
					return nil
				}))
			}
		})
		if !ok {
			t.FailNow()
		}
	}
	s, j = dsgOpenDeletionFixture(t, dir, input)
	completed, err := j.status(t.Context(), p)
	require.NoError(t, err)
	require.Equal(t, p.proof(len(p.stages())-1), completed.TrialProof)
	require.Nil(t, completed.Receipt)
	require.ErrorIs(t, j.advance(t.Context(), p, nil), ErrDSGExactPreserve)
	require.Zero(t, s.calls)
	require.NoError(t, s.db.Close())
	if !t.Run("missing-pre-effect", func(t *testing.T) {
		dsgRejectAmbiguousDeletion(t, false)
	}) || !t.Run("partial-postcondition", func(t *testing.T) {
		dsgRejectAmbiguousDeletion(t, true)
	}) {
		t.FailNow()
	}
	t.Log(dsgNativeDeletionQualification + " snapshot_calls=1 blob_calls=1 recovery_calls=0 non_target_changes=0 production_receipts=0")
}

func dsgRejectAmbiguousDeletion(t *testing.T, partial bool) {
	dir, input := dsgSeedDeletionFixture(t)
	s, j := dsgOpenDeletionFixture(t, dir, input)
	require.NoError(t, j.begin(t.Context(), s.plan))
	require.NoError(t, s.db.Close())
	dsgRunDeletionChild(t, dir, input, "snapshot", "armed", 85)
	s, j = dsgOpenDeletionFixture(t, dir, input)
	defer func() { require.NoError(t, s.db.Close()) }()
	p := s.plan
	require.NoError(t, DSGWithDedicatedMaintenance(s.root, func() error {
		if partial {
			if err := s.recordCall(t.Context(), p, 0); err != nil {
				return err
			}
			// Reproduce the native rename boundary with metadata intact.
			path := filepath.Join(s.root, "snapshots", "snapshots")
			return os.Rename(filepath.Join(path, s.layout.TargetID), filepath.Join(path, "rm-"+s.layout.TargetID))
		}
		// An absent backing blob without an invocation marker is unexplained.
		return os.Rename(dsgBackingBlobPath(filepath.Join(s.root, "content"), s.blob("target")), filepath.Join(filepath.Dir(s.root), "held-blob"))
	}))
	require.ErrorIs(t, j.advance(t.Context(), p, nil), ErrDSGExactPreserve)
	_, err := j.status(t.Context(), p)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	state, err := j.load(p)
	require.NoError(t, err)
	require.True(t, state.Armed)
	require.Zero(t, state.Confirmed)
	require.Empty(t, state.TrialProof)
	require.Nil(t, state.Receipt)
	require.Zero(t, s.calls)
}
