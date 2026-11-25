//go:build linux

package cache

import (
	"context"
	"crypto/sha256"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

func TestDSGGraphCacheRecordClosure(t *testing.T) {
	f := dsgNewGraphFixture(t)
	grandparent, otherGrandparent := strings.Repeat("g", 25), strings.Repeat("h", 25)
	ancestorAlias, secondAlias := strings.Repeat("i", 25), strings.Repeat("j", 25)
	grandchild, sibling := strings.Repeat("k", 25), strings.Repeat("l", 25)
	require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
		put := func(id, key string, value any) {
			dsgGraphTestMetadata(t, dsgGraphTestBucket(t, tx, "_main", id), key, value)
		}
		for _, id := range []string{grandparent, otherGrandparent, sibling} {
			dsgGraphTestBucket(t, tx, "_main", id)
		}
		put(dsgGraphTestParent, keyMergeParents, []string{grandparent, otherGrandparent})
		put(ancestorAlias, keyEqualMutable, dsgGraphTestParent)
		put(secondAlias, keyEqualMutable, dsgOther)
		put(dsgGraphTestChild, keyUpperDiffParent, secondAlias)
		put(dsgGraphTestChild, keyLowerDiffParent, sibling)
		put(grandchild, keyParent, dsgGraphTestChild)
		return nil
	}))
	g := f.observe(t)
	require.Equal(t, &DSGTransitiveCache{
		Records: 9, Ancestors: 4, Descendants: 2, AliasRecords: 2, ParentLinks: 6, AliasLinks: 3,
	}, g.Transitive.CacheRecords)
	require.Equal(t, g, f.observe(t))
	require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
		b := dsgGraphTestBucket(t, tx, "_main", sibling)
		dsgGraphTestMetadata(t, b, keyDescription, "opaque cache metadata")
		return nil
	}))
	require.Equal(t, g, f.observe(t))
	require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
		b := dsgGraphTestBucket(t, tx, "_main", dsgGraphTestParent)
		dsgGraphTestMetadata(t, b, keyMergeParents, []string{otherGrandparent, grandparent})
		return nil
	}))
	reordered := f.observe(t)
	require.Equal(t, g.Transitive.CacheRecords, reordered.Transitive.CacheRecords)
	require.NotEqual(t, g.EvidenceHash, reordered.EvidenceHash)
}

func TestDSGGraphCacheRecordClosurePreservesUnresolvedGraph(t *testing.T) {
	for _, tc := range []struct {
		name, key, value string
	}{
		{"missing-grandparent", keyParent, strings.Repeat("m", 25)},
		{"missing-ancestor-alias", keyEqualMutable, strings.Repeat("m", 25)},
		{"recursive-parent-alias-load", keyEqualMutable, dsgOther},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := dsgNewGraphFixture(t)
			require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
				b := dsgGraphTestBucket(t, tx, "_main", dsgGraphTestParent)
				dsgGraphTestMetadata(t, b, tc.key, tc.value)
				return nil
			}))
			g := f.observe(t)
			require.NotNil(t, g.Cache)
			require.Nil(t, g.Transitive.CacheRecords)
			require.Contains(t, g.Unverified, "cache_record_transitive_parents_and_aliases")
			require.Equal(t, "preserve", g.Disposition)
		})
	}
}

func TestDSGGraphCacheRecordClosureRejectsInvalidShape(t *testing.T) {
	for _, tc := range []struct {
		name string
		edit func(*testing.T, *bolt.Bucket)
	}{
		{"noncanonical-wrapper", func(t *testing.T, b *bolt.Bucket) {
			require.NoError(t, b.Put([]byte(keyParent), []byte(`{"value":"`+dsgTarget+`","index":"opaque"}`)))
		}},
		{"mixed-parent-types", func(t *testing.T, b *bolt.Bucket) {
			dsgGraphTestMetadata(t, b, keyParent, dsgTarget)
			dsgGraphTestMetadata(t, b, keyMergeParents, []string{dsgOther})
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := dsgNewGraphFixture(t)
			require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
				tc.edit(t, dsgGraphTestBucket(t, tx, "_main", dsgGraphTestParent))
				return nil
			}))
			g := f.observe(t)
			require.Nil(t, g.Transitive.CacheRecords)
			require.Contains(t, g.Unverified, "cache_record_transitive_parents_and_aliases")
			require.Equal(t, "preserve", g.Disposition)
		})
	}
}

func TestDSGGraphCacheRecordClosureBoundsReferences(t *testing.T) {
	f := dsgNewGraphFixture(t)
	require.NoError(t, f.cache.View(func(tx *bolt.Tx) error {
		r := dsgGraphReader{ctx: context.Background(), remaining: 3, evidence: sha256.New()}
		g := &DSGDedicatedGraph{Transitive: &DSGGraphTransitive{}}
		_, _, err := r.cache(g, tx, dsgTarget)
		require.ErrorIs(t, err, ErrDSGExactPreserve)
		require.Nil(t, g.Transitive.CacheRecords)
		return nil
	}))
}
