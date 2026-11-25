//go:build linux

package cache

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

func TestDSGSameMountOneShotApproval(t *testing.T) {
	ctx := t.Context()
	stage := t.TempDir()
	require.NoError(t, os.Chmod(stage, 0700))
	path := filepath.Join(stage, "grants.db")
	authority, err := dsgCreateSameMountAuthority(path)
	require.NoError(t, err)
	defer func() { authority.db.Close() }()
	hash := func(s string) string { return dsgRecoveryHash([]byte(s)) }
	g := dsgTestLedgerGrant(authority.device + 1)
	r := g.Request
	require.NoError(t, authority.approve(ctx, g))
	require.NoError(t, authority.admit(ctx, g))
	require.ErrorIs(t, authority.approve(ctx, g), ErrDSGExactPreserve)
	alterations := []func(*dsgSameMountGrant){
		func(v *dsgSameMountGrant) { v.Operation = "other" }, func(v *dsgSameMountGrant) { v.Evidence.Identity.MountID++ },
		func(v *dsgSameMountGrant) { v.Evidence.Identity.UniqueMountID++ }, func(v *dsgSameMountGrant) { v.Evidence.Identity.FSID[0]++ },
		func(v *dsgSameMountGrant) { v.Evidence.Identity.MountNS++ }, func(v *dsgSameMountGrant) { v.Evidence.Record.GraphSHA256 = hash("other") },
		func(v *dsgSameMountGrant) { v.Evidence.Source.PlanSHA256 = hash("other") }, func(v *dsgSameMountGrant) { v.CustodySHA256 = hash("other") },
		func(v *dsgSameMountGrant) { v.Request.Intent = hash("other") }, func(v *dsgSameMountGrant) { v.Request.Approval = hash("other") },
		func(v *dsgSameMountGrant) { v.Request.ExpiresAtMS += 1 }, func(v *dsgSameMountGrant) { v.Evidence.Identity.RootInode++ },
	}
	for _, change := range alterations {
		other := g
		change(&other)
		require.ErrorIs(t, authority.admit(ctx, other), ErrDSGExactPreserve)
		require.ErrorIs(t, authority.advance(ctx, other, nil), ErrDSGExactPreserve)
	}
	other := g
	other.Request.Approval, other.Request.Intent = hash("other approval"), hash("other intent")
	require.ErrorIs(t, authority.approve(ctx, other), ErrDSGExactPreserve)
	require.NoError(t, authority.advance(ctx, g, nil))
	require.NoError(t, authority.db.Close())
	authority, err = dsgOpenSameMountAuthority(path)
	require.NoError(t, err)
	require.ErrorIs(t, authority.advance(ctx, g, nil), ErrDSGExactPreserve)
	require.ErrorIs(t, authority.admit(ctx, g), ErrDSGExactPreserve)
	receipt := DSGExactReceipt{Request: r, LogicalRecords: 1}
	forged := receipt
	forged.Request.Approval = hash("other")
	require.ErrorIs(t, authority.advance(ctx, g, &forged), ErrDSGExactPreserve)
	require.NoError(t, authority.advance(ctx, g, &receipt))
	require.NoError(t, authority.db.View(func(tx *bolt.Tx) error {
		s, err := dsgReadSameMountState(tx, g)
		require.Equal(t, "settled", s.Phase)
		require.Equal(t, &receipt, s.Receipt)
		return err
	}))
	require.ErrorIs(t, authority.advance(ctx, g, &receipt), ErrDSGExactPreserve)
}

func dsgTestLedgerGrant(device uint64) dsgSameMountGrant {
	hash := func(s string) string { return dsgRecoveryHash([]byte(s)) }
	source := dsgSameMountSource{Project: hash("project"), Worker: strings.Repeat("a", 25), RecordID: strings.Repeat("b", 25), PayloadSHA256: hash("payload"), LayerSHA256: hash("layer"), Origin: hash("source"), PlanSHA256: hash("plan"), PayloadGeneration: hash("origin-generation")}
	mount := dsgSameMountIdentity{BootSHA256: hash("boot"), EnrollmentSHA256: hash("enrollment"), MountNS: 1, MountID: 2, UniqueMountID: 3, RootInode: 4, Device: device, FSID: [2]int32{1, 2}}
	e := dsgSameMountEvidence{source, mount, mount.generation(), dsgSameMountRecord{hash("graph"), hash("metadata"), [5]string{hash("1"), hash("2"), hash("3"), hash("4"), hash("5")}}}
	r := DSGExactIntent{Project: source.Project, Worker: source.Worker, Generation: e.Generation, RecordID: source.RecordID, MetadataDigest: e.Record.MetadataDigest, Origin: source.Origin, Intent: hash("intent"), Approval: hash("approval"), ApprovedAtMS: time.Now().Add(-time.Second).UnixMilli(), ExpiresAtMS: time.Now().Add(time.Minute).UnixMilli()}
	return dsgSameMountGrant{Operation: dsgSameMountOperation, CustodySHA256: hash("custody"), Evidence: e, Request: r}
}
