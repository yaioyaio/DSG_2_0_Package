//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/core/content"
	"github.com/containerd/containerd/v2/core/leases"
	ctdmetadata "github.com/containerd/containerd/v2/core/metadata"
	"github.com/containerd/containerd/v2/core/metadata/boltutil"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/moby/buildkit/cache/metadata"
	"github.com/moby/buildkit/identity"
	"github.com/moby/buildkit/solver"
	"github.com/moby/buildkit/solver/bboltcachestorage"
	"github.com/opencontainers/go-digest"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

const dsgSolverEscrow = "_dsg_trial_solver_escrow_v1"

// Only the child that exclusively created this fresh worker seeds its native
// objects. Parent recovery opens only the child directory under its own TempDir.
type dsgNativeTrial struct {
	root                  string
	solverDB, containerDB *bolt.DB
	cacheStore            *metadata.Store
	lm                    leases.Manager
	p                     dsgRecoveryPlan
}

func (s *dsgNativeTrial) result() string { return s.p.Request.Worker + "::" + s.p.Request.RecordID }
func (s *dsgNativeTrial) pin() string    { return "dsg-trial-" + s.p.Request.Intent }
func (s *dsgNativeTrial) resources() []leases.Resource {
	return []leases.Resource{{Type: "snapshots/native", ID: s.p.Objects[2].Key}, {Type: "content", ID: s.p.Objects[3].Key}}
}

func (s *dsgNativeTrial) solverPeers(tx *bolt.Tx) (string, error) {
	node, id := s.p.Objects[0].Key, s.result()
	return dsgTrialTree(tx, func(path []string) bool {
		return slices.Equal(path, []string{dsgSolverEscrow}) ||
			slices.Equal(path, []string{"_result", node, id}) ||
			slices.Equal(path, []string{"_byresult", id, node})
	})
}
func (s *dsgNativeTrial) containerPeers(tx *bolt.Tx) (string, error) {
	return dsgTrialTree(tx, func(path []string) bool {
		return slices.Equal(path, []string{"v1", dsgGraphNamespace, "leases", s.pin()})
	})
}

func (s *dsgNativeTrial) solverState(tx *bolt.Tx) (dsgRecoveryObservation, error) {
	node, id := s.p.Objects[0].Key, s.result()
	peers, err := s.solverPeers(tx)
	if err != nil || peers != s.p.Objects[0].NonTargetSHA256 {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	forward := tx.Bucket([]byte("_result")).Bucket([]byte(node))
	reverse := tx.Bucket([]byte("_byresult")).Bucket([]byte(id))
	if forward == nil || reverse == nil {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	raw, back := forward.Get([]byte(id)), reverse.Get([]byte(node))
	escrow := tx.Bucket([]byte(dsgSolverEscrow))
	if escrow == nil && raw != nil && back != nil && len(back) == 0 && dsgRecoveryHash(raw) == s.p.Objects[0].BeforeSHA256 {
		return dsgRecoveryBefore, nil
	}
	if escrow != nil && raw == nil && back == nil && dsgRecoveryHash(escrow.Get([]byte("preimage"))) == s.p.Objects[0].BeforeSHA256 &&
		string(escrow.Get([]byte("proof"))) == s.p.proof(0) && escrow.Stats().KeyN == 2 {
		return dsgRecoveryAfter, nil
	}
	return dsgRecoveryUnknown, ErrDSGExactPreserve
}

func (s *dsgNativeTrial) leaseState(ctx context.Context, tx *bolt.Tx) (dsgRecoveryObservation, error) {
	peers, err := s.containerPeers(tx)
	if err != nil || peers != s.p.Objects[1].NonTargetSHA256 {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	ctx = boltutil.WithTransaction(ctx, tx)
	resources, err := s.lm.ListResources(ctx, leases.Lease{ID: s.p.Request.RecordID})
	if err != nil || len(resources) != 2 || !slices.Contains(resources, s.resources()[0]) || !slices.Contains(resources, s.resources()[1]) {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	pins, err := s.lm.List(ctx, "id=="+s.pin())
	if err != nil {
		return dsgRecoveryUnknown, err
	}
	if len(pins) == 0 {
		return dsgRecoveryBefore, nil
	}
	if len(pins) != 1 || len(pins[0].Labels) != 1 || pins[0].Labels["dsg.trial.proof"] != s.p.proof(1) {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	resources, err = s.lm.ListResources(ctx, pins[0])
	if err != nil || len(resources) != 2 || !slices.Contains(resources, s.resources()[0]) || !slices.Contains(resources, s.resources()[1]) {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	return dsgRecoveryAfter, nil
}

// Bounded physical inventory of generated fixture files.
func dsgTrialAllocations(root string, n int) (string, error) {
	var names []string
	switch n {
	case 2:
		names = []string{"snapshots/metadata.db", "snapshots/snapshots"}
	case 3:
		names = []string{"blobs/sha256"}
	case 4:
		names = []string{"metadata.db"}
	default:
		return "", ErrDSGExactPreserve
	}
	return dsgTrialFiles(root, names)
}

func dsgTrialFiles(root string, names []string) (string, error) {
	h := sha256.New()
	var walk func(string, int) error
	walk = func(rel string, depth int) error {
		path := filepath.Join(root, rel)
		st, err := os.Lstat(path)
		if err != nil {
			return err
		}
		fmt.Fprintf(h, "%d:%s:%s:", len(rel), rel, st.Mode())
		switch {
		case st.IsDir():
			if depth > 2 {
				return ErrDSGExactPreserve
			}
			entries, err := os.ReadDir(path)
			if err != nil || len(entries) > 8 {
				return ErrDSGExactPreserve
			}
			for _, ent := range entries {
				if err := walk(filepath.Join(rel, ent.Name()), depth+1); err != nil {
					return err
				}
			}
		case st.Mode().IsRegular() && st.Size() >= 0 && st.Size() <= 1<<20:
			f, err := os.Open(path)
			if err != nil {
				return err
			}
			defer f.Close()
			var inode unix.Stat_t
			if err := unix.Fstat(int(f.Fd()), &inode); err != nil {
				return err
			}
			fmt.Fprintf(h, "%d:%d:%d:%d:%d:", st.Size(), inode.Dev, inode.Ino, inode.Mtim.Sec, inode.Mtim.Nsec)
			if _, err := io.CopyN(h, f, st.Size()); err != nil {
				return err
			}
		default:
			return ErrDSGExactPreserve
		}
		return nil
	}
	for _, rel := range names {
		if err := walk(rel, 0); err != nil {
			return "", err
		}
	}
	return fmt.Sprintf("%x", h.Sum(nil)), nil
}

func (s *dsgNativeTrial) observe(ctx context.Context, p dsgRecoveryPlan, n int) (out dsgRecoveryObservation, err error) {
	if p != s.p || p.Mode != dsgNativeTrialMode {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	switch n {
	case 0:
		err = s.solverDB.View(func(tx *bolt.Tx) error { out, err = s.solverState(tx); return err })
	case 1:
		err = s.containerDB.View(func(tx *bolt.Tx) error { out, err = s.leaseState(ctx, tx); return err })
	default:
		return s.observeBacking(ctx, n)
	}
	return out, err
}

func (s *dsgNativeTrial) commit(ctx context.Context, p dsgRecoveryPlan, n int) error {
	if p != s.p || p.Mode != dsgNativeTrialMode {
		return ErrDSGExactPreserve
	}
	switch n {
	case 0:
		return s.solverDB.Update(func(tx *bolt.Tx) error {
			state, err := s.solverState(tx)
			if err != nil || state != dsgRecoveryBefore || !dsgRecoveryAuthorized(ctx, p) {
				return ErrDSGExactPreserve
			}
			node, id := []byte(p.Objects[0].Key), []byte(s.result())
			forward := tx.Bucket([]byte("_result")).Bucket(node)
			escrow, err := tx.CreateBucket([]byte(dsgSolverEscrow))
			if err != nil {
				return err
			}
			if err := escrow.Put([]byte("preimage"), bytes.Clone(forward.Get(id))); err != nil {
				return err
			}
			if err := escrow.Put([]byte("proof"), []byte(p.proof(0))); err != nil {
				return err
			}
			if err := forward.Delete(id); err != nil {
				return err
			}
			return tx.Bucket([]byte("_byresult")).Bucket(id).Delete(node)
		})
	case 1:
		return s.containerDB.Update(func(tx *bolt.Tx) error {
			state, err := s.leaseState(ctx, tx)
			if err != nil || state != dsgRecoveryBefore || !dsgRecoveryAuthorized(ctx, p) {
				return ErrDSGExactPreserve
			}
			ctx := boltutil.WithTransaction(ctx, tx)
			pin, err := s.lm.Create(ctx, leases.WithID(s.pin()), leases.WithLabels(map[string]string{"dsg.trial.proof": p.proof(1)}))
			if err != nil {
				return err
			}
			for _, r := range s.resources() {
				if err := s.lm.AddResource(ctx, pin, r); err != nil {
					return err
				}
			}
			return nil
		})
	default:
		return s.commitBacking(ctx, n)
	}
}

func dsgOpenNativeTrial(t *testing.T, root string, p dsgRecoveryPlan) (*dsgNativeTrial, *dsgRecoveryJournal) {
	t.Helper()
	open := func(name string) *bolt.DB {
		path := filepath.Join(root, name)
		_, err := os.Lstat(path)
		require.NoError(t, err)
		db, err := bolt.Open(path, 0600, &bolt.Options{Timeout: time.Second})
		require.NoError(t, err)
		return db
	}
	s := &dsgNativeTrial{root: root, p: p, solverDB: open("cache.db"), containerDB: open("containerdmeta.db")}
	_, err := os.Lstat(filepath.Join(root, "metadata.db"))
	require.NoError(t, err)
	s.cacheStore, err = metadata.NewStore(filepath.Join(root, "metadata.db"))
	require.NoError(t, err)
	s.lm = ctdmetadata.NewLeaseManager(ctdmetadata.NewDB(s.containerDB, nil, nil))
	j := &dsgRecoveryJournal{root: root, qualificationMode: dsgNativeTrialMode, db: open("trial-journal.db")}
	for n := range j.store {
		j.store[n] = s
	}
	return s, j
}
func dsgCloseNativeTrial(t *testing.T, s *dsgNativeTrial, j *dsgRecoveryJournal) {
	t.Helper()
	require.NoError(t, s.cacheStore.Close())
	require.NoError(t, s.solverDB.Close())
	require.NoError(t, s.containerDB.Close())
	require.NoError(t, j.db.Close())
}

func dsgSeedNativeTrial(t *testing.T, ctx context.Context, root string) (*dsgNativeTrial, *dsgRecoveryJournal) {
	t.Helper()
	sn, err := native.NewSnapshotter(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	worker, closeWorker, err := newCacheManager(ctx, t, cmOpt{tmpdir: root, snapshotter: sn, snapshotterName: "native"})
	require.NoError(t, err)
	now := time.Now().UTC()
	workerID := identity.NewID()
	p := dsgRecoveryPlan{Mode: dsgNativeTrialMode, Request: DSGExactIntent{
		Worker: workerID, Project: dsgRecoveryHash([]byte("trial project")), Generation: dsgRecoveryHash([]byte(identity.NewID())),
		Origin: dsgRecoveryHash([]byte("trial child")), Approval: dsgRecoveryHash([]byte(identity.NewID())), Intent: dsgRecoveryHash([]byte(identity.NewID())),
		ApprovedAtMS: now.Add(-time.Second).UnixMilli(), ExpiresAtMS: now.Add(2 * time.Minute).UnixMilli(),
	}}
	db, err := bboltcachestorage.NewStore(filepath.Join(root, "cache.db"))
	require.NoError(t, err)
	for n := 0; n < 2; n++ {
		record, err := worker.manager.New(ctx, nil, nil, CachePolicyDefault)
		require.NoError(t, err)
		id := record.ID()
		data := []byte("generated trial payload " + id)
		dg := digest.FromBytes(data)
		desc := ocispec.Descriptor{Digest: dg, Size: int64(len(data))}
		require.NoError(t, content.WriteBlob(leases.WithLease(ctx, id), worker.cs, id, bytes.NewReader(data), desc))
		mounts, err := worker.testSnapshotter.Mounts(ctx, id)
		require.NoError(t, err)
		require.Len(t, mounts, 1)
		require.Equal(t, filepath.Join(root, "snapshots", "snapshots"), filepath.Dir(mounts[0].Source))
		file := filepath.Join(mounts[0].Source, "trial-payload")
		dsgBackingWrite(t, file, data)
		_, err = worker.cs.Info(ctx, dg)
		require.NoError(t, err)
		node := digest.FromString("trial node " + id).String()
		res := solver.CacheResult{ID: workerID + "::" + id, CreatedAt: now}
		require.NoError(t, db.AddResult(node, res))
		if n == 0 {
			p.Request.RecordID = id
			raw, err := json.Marshal(res)
			require.NoError(t, err)
			p.Objects[0] = dsgRecoveryObject{Key: node, BeforeSHA256: dsgRecoveryHash(raw)}
			p.Objects[1].Key = id
			p.Objects[2].Key = id
			p.Objects[3].Key = dg.String()
			p.Objects[4].Key = id
			// Seed the staging allocation with the actual target file. The
			// original worker snapshot and its lease continue to resolve.
			allocation := dsgBackingPrepare(t, ctx, sn, root, dsgTrialActive(p), "")
			copyData, err := os.ReadFile(file)
			require.NoError(t, err)
			dsgBackingWrite(t, dsgBackingSnapshotPath(root, allocation, "trial-payload"), copyData)
		}
	}
	require.NoError(t, db.Close())
	closeWorker()
	require.NoError(t, sn.Close())
	journal, err := bolt.Open(filepath.Join(root, "trial-journal.db"), 0600, nil)
	require.NoError(t, err)
	require.NoError(t, journal.Close())
	s, j := dsgOpenNativeTrial(t, root, p)
	require.NoError(t, s.solverDB.View(func(tx *bolt.Tx) error { p.Objects[0].NonTargetSHA256, err = s.solverPeers(tx); return err }))
	require.NoError(t, s.containerDB.View(func(tx *bolt.Tx) error { p.Objects[1].NonTargetSHA256, err = s.containerPeers(tx); return err }))
	p.Objects[1].BeforeSHA256 = p.Objects[1].NonTargetSHA256
	p.Objects[2], _, err = s.snapshotEvidence()
	require.NoError(t, err)
	p.Objects[3], _, err = s.blobEvidence()
	require.NoError(t, err)
	require.NoError(t, s.cacheStore.DB().View(func(tx *bolt.Tx) error {
		g, err := s.cacheGrant(tx)
		if err != nil {
			return err
		}
		p.Objects[4].NonTargetSHA256, err = g.peers(tx)
		if err != nil {
			return err
		}
		p.Objects[4].BeforeSHA256, err = dsgTrialTree(tx, func([]string) bool { return false })
		return err
	}))
	p.Request.MetadataDigest = p.Objects[4].BeforeSHA256
	raw, err := json.Marshal(p.Objects)
	require.NoError(t, err)
	p.GraphSHA256 = dsgRecoveryHash(raw)
	s.p = p
	return s, j
}

func TestDSGNativeJournalRecovery(t *testing.T) {
	ctx := namespaces.WithNamespace(t.Context(), dsgGraphNamespace)
	args := os.Args
	if len(args) > 2 && args[len(args)-2] == "dsg-native-journal-effect" {
		stage := slices.Index(dsgRecoveryDomains[:], args[len(args)-1])
		require.NotEqual(t, -1, stage)
		root, err := os.Readlink("/proc/self/fd/3")
		require.NoError(t, err)
		var p dsgRecoveryPlan
		require.NoError(t, json.NewDecoder(io.LimitReader(os.Stdin, 8192)).Decode(&p))
		require.True(t, p.valid(dsgNativeTrialMode))
		s, j := dsgOpenNativeTrial(t, root, p)
		state, err := j.status(ctx, s.p)
		require.NoError(t, err)
		require.False(t, state.Armed)
		require.Equal(t, stage, state.Confirmed)
		err = j.advance(ctx, p, func(point string) {
			if point == "effect" {
				os.Exit(86)
			}
		})
		t.Fatalf("expected exit immediately after native effect, got %v", err)
	}

	parent := t.TempDir()
	root := filepath.Join(parent, "fresh-native-trial")
	require.NoError(t, os.Mkdir(root, 0700))
	var fs unix.Statfs_t
	require.NoError(t, unix.Statfs(root, &fs))
	require.Equal(t, int64(unix.EXT4_SUPER_MAGIC), int64(fs.Type))
	dir, err := os.Open(root)
	require.NoError(t, err)
	defer dir.Close()
	s, j := dsgSeedNativeTrial(t, ctx, root)
	p := s.p
	require.NoError(t, j.begin(ctx, p))
	dsgCloseNativeTrial(t, s, j)
	raw, err := json.Marshal(p)
	require.NoError(t, err)
	exe, err := os.Executable()
	require.NoError(t, err)
	for stage, domain := range dsgRecoveryDomains {
		ok := t.Run(domain, func(t *testing.T) {
			childCtx, cancel := context.WithTimeout(ctx, 25*time.Second)
			defer cancel()
			cmd := exec.CommandContext(childCtx, exe, "-test.run=^TestDSGNativeJournalRecovery$", "--", "dsg-native-journal-effect", domain)
			cmd.Env = []string{"TMPDIR=" + parent}
			cmd.ExtraFiles = []*os.File{dir}
			cmd.Stdin = bytes.NewReader(raw)
			out, err := cmd.CombinedOutput()
			require.NotNil(t, cmd.ProcessState)
			if cmd.ProcessState.ExitCode() != 86 {
				lines := regexp.MustCompile(`dsg_native_[a-z_]+_test\.go:[0-9]+`).FindAll(out, 6)
				t.Fatalf("native child failed: %v (%s)", err, lines)
			}
			s, j := dsgOpenNativeTrial(t, root, p)
			defer dsgCloseNativeTrial(t, s, j)
			state, err := j.status(ctx, p)
			require.NoError(t, err)
			require.True(t, state.Armed)
			require.Equal(t, stage, state.Confirmed)
			require.Nil(t, state.Receipt)
			require.Empty(t, state.TrialProof)
			var barriers []string
			require.NoError(t, j.advance(ctx, p, func(point string) { barriers = append(barriers, point) }))
			require.Equal(t, []string{"ack"}, barriers)
			state, err = j.status(ctx, p)
			require.NoError(t, err)
			require.False(t, state.Armed)
			require.Equal(t, stage+1, state.Confirmed)
			require.Nil(t, state.Receipt)
			if stage+1 == len(p.Objects) {
				require.Equal(t, p.proof(stage), state.TrialProof)
				fabricated := state
				fabricated.TrialProof = ""
				fabricated.Receipt = &DSGExactReceipt{Request: p.Request, LogicalRecords: 1}
				require.False(t, fabricated.valid(p))
				require.ErrorIs(t, j.advance(ctx, p, nil), ErrDSGExactPreserve)
				require.ErrorIs(t, j.begin(ctx, p), ErrDSGExactPreserve)
				target, ok := s.cacheStore.Get(p.Request.RecordID)
				require.True(t, ok)
				require.True(t, (&cacheMetadata{si: target}).HasCachePolicyRetain())
			} else {
				require.Empty(t, state.TrialProof)
			}
		})
		if !ok {
			t.FailNow()
		}
	}
	db, err := bboltcachestorage.NewStore(filepath.Join(root, "cache.db"))
	require.NoError(t, err)
	_, err = db.Load(p.Objects[0].Key, p.Request.Worker+"::"+p.Request.RecordID)
	require.ErrorIs(t, err, solver.ErrNotFound)
	seen := 0
	require.NoError(t, db.WalkIDsByResult(p.Request.Worker+"::"+p.Request.RecordID, func(string) error { seen++; return nil }))
	require.Zero(t, seen)
	require.NoError(t, db.Walk(func(node string) error {
		return db.WalkResults(node, func(r solver.CacheResult) error { seen++; return nil })
	}))
	require.Equal(t, 1, seen)
	require.NoError(t, db.Close())
	t.Log(dsgNativeTrialMode)
}
