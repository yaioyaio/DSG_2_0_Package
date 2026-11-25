//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"hash"
	"path/filepath"
	"strings"
	"testing"

	controlapi "github.com/moby/buildkit/api/services/control"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
	"google.golang.org/protobuf/encoding/protowire"
)

const dsgExternalPrivate = "synthetic-private-value"

func dsgNewExternalHistory(t *testing.T, version string) *bolt.DB {
	t.Helper()
	db, err := bolt.Open(filepath.Join(t.TempDir(), "history.db"), 0600, nil)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, db.Close()) })
	require.NoError(t, db.Update(func(tx *bolt.Tx) error {
		return dsgGraphTestBucket(t, tx, "_version").Put([]byte("version"), []byte(version))
	}))
	return db
}

func dsgWriteExternalHistory(t *testing.T, db *bolt.DB, rec *controlapi.BuildHistoryRecord, suffix []byte) {
	t.Helper()
	raw, err := rec.MarshalVT()
	require.NoError(t, err)
	require.NoError(t, db.Update(func(tx *bolt.Tx) error {
		return dsgGraphTestBucket(t, tx, "_records").Put([]byte(rec.Ref), append(raw, suffix...))
	}))
}

type dsgExternalTestEvidence struct {
	hash.Hash
	trace bytes.Buffer
}

func (h *dsgExternalTestEvidence) Write(p []byte) (int, error) {
	h.trace.Write(p)
	return h.Hash.Write(p)
}

func dsgRunExternalTest(t *testing.T, fixture *dsgGraphFixture, history *bolt.DB, budget int) (*DSGGraphExternal, []string, string) {
	t.Helper()
	open := func(db *bolt.DB) *bolt.Tx {
		tx, err := db.Begin(false)
		require.NoError(t, err)
		return tx
	}
	cache, backend := open(fixture.cache), open(fixture.backend)
	defer cache.Rollback()
	defer backend.Rollback()
	var ht *bolt.Tx
	if history != nil {
		ht = open(history)
		defer ht.Rollback()
	}
	evidence := &dsgExternalTestEvidence{Hash: sha256.New()}
	r := &dsgGraphReader{ctx: context.Background(), remaining: budget, evidence: evidence}
	o, unverified := r.externalRefs(cache, backend, ht, dsgTarget, dsgTarget, "sha256:"+strings.Repeat("a", 64))
	return o, unverified, evidence.trace.String()
}

func dsgExternalTestWireField(n protowire.Number, v []byte) []byte {
	return protowire.AppendBytes(protowire.AppendTag(nil, n, protowire.BytesType), v)
}

func TestDSGGraphExternalPinnedStructuralAndHistory(t *testing.T) {
	f := dsgNewGraphFixture(t)
	blob := "sha256:" + strings.Repeat("a", 64)
	other := "sha256:" + strings.Repeat("b", 64)
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		for _, image := range []struct{ namespace, digest string }{{"buildkit", blob}, {"other", other}} {
			target := dsgGraphTestBucket(t, tx, "v1", image.namespace, "images", dsgExternalPrivate, "target")
			require.NoError(t, target.Put([]byte("digest"), []byte(image.digest)))
		}
		for _, c := range []struct{ namespace, key, snapshotter string }{
			{"buildkit", dsgTarget, "native"}, {"other", dsgOther, "native"}, {"unmapped", dsgTarget, "native"}, {"different", dsgTarget, "overlayfs"},
		} {
			container := dsgGraphTestBucket(t, tx, "v1", c.namespace, "containers", dsgExternalPrivate)
			require.NoError(t, container.Put([]byte("snapshotter"), []byte(c.snapshotter)))
			require.NoError(t, container.Put([]byte("snapshotKey"), []byte(c.key)))
			require.NoError(t, container.Put([]byte("spec"), []byte(dsgExternalPrivate)))
		}
		for _, ingest := range []struct{ name, digest string }{{"matching", blob}, {"unconstrained", ""}} {
			b := dsgGraphTestBucket(t, tx, "v1", "other", "content", "ingests", ingest.name)
			require.NoError(t, b.Put([]byte("expected"), []byte(ingest.digest)))
			require.NoError(t, b.Put([]byte("ref"), []byte(dsgExternalPrivate)))
		}
		return nil
	}))
	contentHashRecord := append(dsgExternalTestWireField(1, []byte(blob)), dsgExternalTestWireField(3, []byte(dsgExternalPrivate))...)
	contentHashPath := append(dsgExternalTestWireField(1, []byte(dsgExternalPrivate)), dsgExternalTestWireField(2, contentHashRecord)...)
	require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
		target := dsgGraphTestBucket(t, tx, "_external", dsgTarget)
		require.NoError(t, target.Put([]byte(keyFileList), []byte(`["synthetic-private-value","path with a quote \""]`)))
		return dsgGraphTestBucket(t, tx, "_external", dsgOther).Put([]byte("buildkit.contenthash.v0"), dsgExternalTestWireField(1, contentHashPath))
	}))
	history := dsgNewExternalHistory(t, "2")
	wanted := &controlapi.Descriptor{Digest: blob, Annotations: map[string]string{"annotation": dsgExternalPrivate}}
	missing := &controlapi.Descriptor{Digest: other}
	dsgWriteExternalHistory(t, history, &controlapi.BuildHistoryRecord{
		Ref: dsgExternalPrivate, Logs: wanted, Trace: missing, ExternalError: wanted,
		FrontendAttrs: map[string]string{"private": dsgExternalPrivate},
		Result: &controlapi.BuildResultInfo{
			ResultDeprecated: wanted,
			Results:          map[int64]*controlapi.Descriptor{1: missing, 0: wanted},
			Attestations:     []*controlapi.Descriptor{wanted},
		},
		Results: map[string]*controlapi.BuildResultInfo{dsgExternalPrivate: {Results: map[int64]*controlapi.Descriptor{0: wanted}}},
	}, nil)
	o, unverified, evidence := dsgRunExternalTest(t, f, history, dsgGraphMaxEntries)
	require.Empty(t, unverified)
	require.Equal(t, &DSGGraphExternal{
		ImageObjects: 2, ImageTargetReferences: 1, ContainerObjects: 4, ContainerSnapshotReferences: 2,
		IngestObjects: 2, IngestExpectedReferences: 1,
		HistoryRecords: 1, HistoryDescriptors: 8, HistoryDescriptorReferences: 6,
		CacheFileLists: 1, CacheContentHashes: 1, TargetCacheExternalEntries: 1,
	}, o)
	require.NotContains(t, evidence, dsgExternalPrivate)
	require.NotContains(t, evidence, other)
	_, _, replay := dsgRunExternalTest(t, f, history, dsgGraphMaxEntries)
	require.Equal(t, evidence, replay)
}

func TestDSGGraphExternalPreservesOpaqueAndUnknown(t *testing.T) {
	f := dsgNewGraphFixture(t)
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		dsgGraphTestBucket(t, tx, "v1", "buildkit", "content", "ingests", "object", "expected")
		return nil
	}))
	require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
		target := dsgGraphTestBucket(t, tx, "_external", dsgTarget)
		require.NoError(t, target.Put([]byte(keyFileList), []byte(`[null]`)))
		require.NoError(t, target.Put([]byte("buildkit.contenthash.v0"), []byte{0xff}))
		return dsgGraphTestBucket(t, tx, "_external", dsgOther).Put([]byte(dsgExternalPrivate), []byte(dsgExternalPrivate))
	}))
	history := dsgNewExternalHistory(t, "2")
	unknownField := dsgExternalTestWireField(20, []byte(dsgExternalPrivate))
	dsgWriteExternalHistory(t, history, &controlapi.BuildHistoryRecord{Ref: "bad"}, unknownField)
	dsgWriteExternalHistory(t, history, &controlapi.BuildHistoryRecord{
		Ref: "valid", Trace: &controlapi.Descriptor{Digest: "sha256:" + strings.Repeat("a", 64)},
	}, nil)
	o, unverified, evidence := dsgRunExternalTest(t, f, history, dsgGraphMaxEntries)
	require.Equal(t, []string{"containerd_image_container_or_ingest_projection", "opaque_build_history_records", "opaque_external_cache_metadata"}, unverified)
	require.Equal(t, 2, o.HistoryRecords)
	require.Equal(t, 1, o.OpaqueHistoryRecords)
	require.Equal(t, 1, o.HistoryDescriptorReferences)
	require.Equal(t, 3, o.CacheOpaqueEntries)
	require.Equal(t, 2, o.TargetCacheExternalEntries)
	require.NotContains(t, evidence, dsgExternalPrivate)
}

func TestDSGGraphExternalRequiresPinnedHistoryAndBudget(t *testing.T) {
	f := dsgNewGraphFixture(t)
	for _, test := range []struct {
		name    string
		history *bolt.DB
		budget  int
	}{
		{"missing", nil, dsgGraphMaxEntries},
		{"schema", dsgNewExternalHistory(t, "3"), dsgGraphMaxEntries},
		{"budget", dsgNewExternalHistory(t, "2"), 0},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, unverified, _ := dsgRunExternalTest(t, f, test.history, test.budget)
			require.Contains(t, unverified, "build_history_projection")
		})
	}
}
