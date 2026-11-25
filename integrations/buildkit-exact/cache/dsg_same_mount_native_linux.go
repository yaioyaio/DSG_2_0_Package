//go:build linux

package cache

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/opencontainers/go-digest"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

// input comes from the protected registration captured when the empty, isolated
// worker was populated. No pathname, bucket or partial ID comes from the grant.
type dsgSameMountNative struct {
	input dsgReclaimInput
	// The public supervisor pins the issuing keeper across every native step.
	keeperAlive func() bool
}

func dsgSameMountNativeSource(in dsgReclaimInput) dsgSameMountSource {
	p := in.Plan
	payload := (&dsgDeletionFixture{plan: p}).payload("snapshot", "target")
	return dsgSameMountSource{
		Project: p.Request.Project, Worker: p.Request.Worker, RecordID: p.Request.RecordID,
		PayloadGeneration: p.Request.Generation, PayloadSHA256: dsgRecoveryHash([]byte(payload)),
		Origin:      dsgRecoveryHash(append([]byte("manifestless-native-payload-v1\x00"), []byte(payload)...)),
		LayerSHA256: strings.TrimPrefix(in.Layout.Blobs[0].Digest.String(), "sha256:"),
		PlanSHA256:  dsgDeletionHash(in),
	}
}

func (n dsgSameMountNative) open(fd int, source dsgSameMountSource) (*dsgReclaimFixture, error) {
	in := n.input
	p, shape := in.Plan, in.Layout
	if !p.valid(dsgNativeReclamationQualification) || source != dsgSameMountNativeSource(in) ||
		p.Request.Origin != source.Origin || p.GraphSHA256 != dsgDeletionHash(shape) ||
		p.Objects[0].Key != "sha256:"+source.Origin || p.Objects[1].Key != source.RecordID ||
		p.Objects[2].Key != shape.Snapshot || p.Objects[3].Key != "sha256:"+source.LayerSHA256 ||
		p.View.Key != digest.FromString("native view/"+shape.Snapshot+"-view").String() ||
		!dsgGraphSHA256(shape.Blobs[0].Digest.String()) || !dsgGraphSHA256(shape.Blobs[1].Digest.String()) ||
		shape.Blobs[0].Digest == shape.Blobs[1].Digest || shape.PeerRecord == source.RecordID ||
		shape.Backing.PeerID == shape.Backing.TargetID || !dsgFullID.MatchString(shape.PeerRecord) ||
		!dsgDigest.MatchString(shape.Graph) {
		return nil, ErrDSGExactPreserve
	}
	var fs unix.Statfs_t
	if unix.Fstatfs(fd, &fs) != nil || fs.Type != unix.EXT4_SUPER_MAGIC || !dsgValidMarker(fd) {
		return nil, ErrDSGExactPreserve
	}
	child, err := unix.Openat2(fd, dsgDeletionRoot, &unix.OpenHow{
		Flags:   unix.O_RDONLY | unix.O_DIRECTORY | unix.O_CLOEXEC,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS,
	})
	if err != nil {
		return nil, ErrDSGExactPreserve
	}
	dir := os.NewFile(uintptr(child), "dsg-registered-native-store")
	root, err := os.Readlink(fmt.Sprintf("/proc/self/fd/%d", child))
	if err != nil || !filepath.IsAbs(root) || !dsgProtectedParent(root) {
		dir.Close()
		return nil, ErrDSGExactPreserve
	}
	s := &dsgReclaimFixture{&dsgDeletionFixture{dir: dir, root: root, plan: p, layout: shape.Backing,
		payloadGeneration: source.PayloadGeneration, readOnly: fs.Flags&unix.ST_RDONLY != 0}, shape}
	if err := s.guard(); err != nil {
		dir.Close()
		return nil, err
	}
	return s, nil
}

// Hold all five read transactions simultaneously, including the empty history
// store. The snapshot and content observations resolve beneath the same dirfd.
func (s *dsgReclaimFixture) graph(ctx context.Context) (*DSGDedicatedGraph, error) {
	var txs [5]*bolt.Tx
	for i, name := range []string{reclaimSolver, reclaimCache, reclaimBackend, reclaimNative, reclaimHistory} {
		db, err := bolt.Open(filepath.Join(s.root, name), 0600, &bolt.Options{ReadOnly: true, Timeout: time.Second})
		if err != nil {
			return nil, err
		}
		defer db.Close()
		txs[i], err = db.Begin(false)
		if err != nil {
			return nil, err
		}
		defer txs[i].Rollback()
	}
	sn, err := dsgOpenOwned(int(s.dir.Fd()), "snapshots", true)
	if err != nil {
		return nil, err
	}
	defer sn.Close()
	return dsgObserveNativeGraph(ctx, s.plan.Request.Worker, s.plan.Request.RecordID, txs[0], txs[1], txs[2], txs[3], int(s.dir.Fd()), int(sn.Fd()), txs[4])
}

func (n dsgSameMountNative) evidence(ctx context.Context, fd int, source dsgSameMountSource) (dsgSameMountRecord, error) {
	var record dsgSameMountRecord
	s, err := n.open(fd, source)
	if err != nil {
		return record, dsgSameMountFailure("native_evidence_open", err)
	}
	defer s.dir.Close()
	g, err := s.graph(ctx)
	if err != nil {
		return record, dsgSameMountFailure("native_graph_open", err)
	}
	if g == nil || g.EvidenceHash != s.shape.Graph || g.Ownership == nil || !g.Ownership.Checked ||
		!g.Ownership.Exclusive || !g.Ownership.RecordLease || !g.Ownership.VariantLease ||
		g.Ownership.OwnedChildSnapshots != 1 || g.Ownership.IncomingContentKeySHA256 != "" ||
		g.Solver == nil || g.Solver.ResultNodes != 1 || g.Solver.ChildLinks+g.Solver.ParentLinks != 0 ||
		g.Allocations == nil || g.Allocations.Blob == nil || g.Allocations.Blob.DeclaredSHA256 != source.LayerSHA256 ||
		!slices.Equal(g.Unverified, []string{"native_allocation_contents"}) {
		return record, dsgSameMountFailure("native_graph_mismatch", nil)
	}
	for stage := range s.plan.stages() {
		target, peer, err := s.evidence(ctx, stage)
		obj := s.plan.object(stage)
		if err != nil || target != obj.BeforeSHA256 || peer != obj.NonTargetSHA256 {
			return record, dsgSameMountFailure("native_preimage_"+s.plan.stages()[stage], err)
		}
	}
	dbs, err := dsgSameMountDatabaseHashes(ctx, int(s.dir.Fd()))
	if err != nil {
		return record, dsgSameMountFailure("native_database_hash", err)
	}
	return dsgSameMountRecord{GraphSHA256: g.EvidenceHash, MetadataDigest: s.plan.Request.MetadataDigest, DatabasesSHA256: dbs}, nil
}

func (n dsgSameMountNative) withJournal(ctx context.Context, fd int, g dsgSameMountGrant, write bool) (DSGExactReceipt, error) {
	var none DSGExactReceipt
	source := g.Evidence.Source
	req := g.Request
	if !g.valid() || req.MetadataDigest != n.input.Plan.Request.MetadataDigest || g.Evidence.Record.GraphSHA256 != n.input.Layout.Graph {
		return none, dsgSameMountFailure("native_grant_binding", nil)
	}
	s, err := n.open(fd, source)
	if err != nil {
		return none, dsgSameMountFailure("native_journal_root", err)
	}
	defer s.dir.Close()
	if write && s.readOnly {
		return none, dsgSameMountFailure("native_writable_mount", nil)
	}
	p := n.input.Plan
	p.Mode = dsgNativeSameMount
	p.Request = req
	// The operational proof covers the full registration, mount and observations,
	// alongside all six native selectors and their exact before/peer digests.
	p.GraphSHA256 = dsgDeletionHash(g)
	s.plan = p
	if write {
		got, err := n.evidence(ctx, fd, source)
		if err != nil {
			return none, err
		}
		if got != g.Evidence.Record {
			return none, dsgSameMountFailure("native_approval_preimage", nil)
		}
	}
	// guard() above requires this pre-created journal inode and its single link.
	s.db, err = bolt.Open(filepath.Join(s.root, dsgDeletionJournal), 0600, &bolt.Options{ReadOnly: !write, Timeout: time.Second})
	if err != nil {
		return none, dsgSameMountFailure("native_journal_open", err)
	}
	defer s.db.Close()
	j := &dsgRecoveryJournal{root: s.root, qualificationMode: dsgNativeSameMount, db: s.db}
	// The supervisor owns the flock on the enrolled parent, including while
	// these stores are open. Re-pin that exact mount before and after each
	// journal transition. Full external-handle scans bracket the native call.
	root := filepath.Dir(s.root)
	j.verifyNativeFence = func() error {
		if n.keeperAlive != nil && !n.keeperAlive() {
			return ErrDSGExactPreserve
		}
		pin, err := dsgSameMountID(fd, root)
		if err != nil || pin != g.Evidence.Identity {
			return ErrDSGExactPreserve
		}
		if _, err = dsgFilesystem(fd, root, s.readOnly); err != nil {
			return err
		}
		return s.guard()
	}
	for i := range j.store {
		j.store[i] = s
	}
	if write {
		if err = j.begin(ctx, p); err != nil {
			return none, dsgSameMountFailure("native_begin", err)
		}
		for _, domain := range p.stages() {
			if err = j.advance(ctx, p, nil); err != nil {
				return none, dsgSameMountFailure("native_advance_"+domain, err)
			}
		}
	}
	state, err := j.status(ctx, p)
	if err != nil {
		return none, dsgSameMountFailure("native_receipt_status", err)
	}
	if state.Confirmed != len(p.stages()) || state.Armed || state.NativeProof != p.proof(len(p.stages())-1) ||
		state.TrialProof != "" || state.Receipt == nil || state.Receipt.Request != req ||
		state.Receipt.LogicalRecords != 1 || state.Receipt.PhysicalBytes != nil {
		return none, dsgSameMountFailure("native_receipt_proof", nil)
	}
	return *state.Receipt, nil
}

func (n dsgSameMountNative) apply(ctx context.Context, fd int, g dsgSameMountGrant) (DSGExactReceipt, error) {
	return n.withJournal(ctx, fd, g, true)
}

func (n dsgSameMountNative) receipt(ctx context.Context, fd int, g dsgSameMountGrant) (DSGExactReceipt, error) {
	return n.withJournal(ctx, fd, g, false)
}
