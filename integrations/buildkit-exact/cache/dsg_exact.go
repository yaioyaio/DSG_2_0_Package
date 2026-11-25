// DSG overlay for github.com/moby/buildkit v0.35.1.
package cache

import (
	"context"
	"errors"
	"regexp"

	"github.com/containerd/containerd/v2/core/leases"
	"github.com/containerd/errdefs"
	digest "github.com/opencontainers/go-digest"
)

var (
	dsgFullID           = regexp.MustCompile("^[0-9a-z]{25}$")
	dsgDigest           = regexp.MustCompile("^[0-9a-f]{64}$")
	ErrDSGExactPreserve = errors.New("exact effects unproven: preserve")
)

// DSGExactObservation counts edges in the cache manager's memory graph. It is
// deliberately insufficient to authorize a write to the worker or its backing.
type DSGExactObservation struct {
	RecordID, SnapshotID, Blob                                       string
	ActiveRefs, Parents, Children, Aliases, SnapshotPeers, BlobPeers int
	Mutable, Dead, Mounted                                           bool
}

// DSGInspectExact never calls Get, DiskUsage, Prune, or any storage operation.
func DSGInspectExact(ctx context.Context, manager Manager, id string) (DSGExactObservation, error) {
	cm, ok := manager.(*cacheManager)
	if !ok || cm == nil || !dsgFullID.MatchString(id) {
		return DSGExactObservation{}, ErrDSGExactPreserve
	}
	cm.mu.Lock()
	defer cm.mu.Unlock()
	return cm.dsgInspectLocked(ctx, id)
}

func (cm *cacheManager) dsgInspectLocked(ctx context.Context, id string) (DSGExactObservation, error) {
	target := cm.records[id]
	if target == nil || target.mu == nil || target.cacheMetadata == nil || target.si == nil || len(cm.records) > 4096 || ctx.Err() != nil {
		return DSGExactObservation{}, ErrDSGExactPreserve
	}
	target.mu.Lock()
	if len(target.mergeParents) > 64 {
		target.mu.Unlock()
		return DSGExactObservation{}, ErrDSGExactPreserve
	}
	o := DSGExactObservation{
		RecordID: target.ID(), SnapshotID: target.getSnapshotID(), Blob: string(target.getBlob()),
		ActiveRefs: len(target.refs), Mutable: target.mutable, Dead: target.isDead(), Mounted: target.mountCache != nil,
	}
	o.Parents, _ = dsgParentEdges(target, id)
	if target.equalMutable != nil {
		o.Aliases++
	}
	if target.equalImmutable != nil {
		o.Aliases++
	}
	target.mu.Unlock()
	if o.RecordID != id {
		return DSGExactObservation{}, ErrDSGExactPreserve
	}
	for key, cr := range cm.records {
		if ctx.Err() != nil || cr == nil || cr.mu == nil || cr.cacheMetadata == nil || cr.si == nil {
			return DSGExactObservation{}, ErrDSGExactPreserve
		}
		if key == id {
			continue
		}
		// Mutable/immutable aliases can share a mutex. Lock one record at a time.
		cr.mu.Lock()
		if len(cr.mergeParents) > 64 {
			cr.mu.Unlock()
			return DSGExactObservation{}, ErrDSGExactPreserve
		}
		_, incoming := dsgParentEdges(cr, id)
		o.Children += incoming
		if cr.equalMutable != nil && cr.equalMutable.ID() == id {
			o.Aliases++
		}
		if cr.equalImmutable != nil && cr.equalImmutable.ID() == id {
			o.Aliases++
		}
		if cr.getSnapshotID() == o.SnapshotID {
			o.SnapshotPeers++
		}
		if o.Blob != "" && string(cr.getBlob()) == o.Blob {
			o.BlobPeers++
		}
		cr.mu.Unlock()
	}
	return o, nil
}

func dsgParentEdges(cr *cacheRecord, id string) (total, matching int) {
	count := func(ref *immutableRef) {
		if ref != nil {
			total++
			if ref.ID() == id {
				matching++
			}
		}
	}
	count(cr.layerParent)
	for _, ref := range cr.mergeParents {
		count(ref)
	}
	if cr.diffParents != nil {
		count(cr.diffParents.lower)
		count(cr.diffParents.upper)
	}
	return total, matching
}

// DSGExactBackingObservation records point reads in the worker's own stores.
// Independent reads cannot prove global absence of incoming references.
type DSGExactBackingObservation struct {
	SnapshotPresent, RecordLeasePresent, BlobPresent bool
	LeaseResourceCount                               int
}

func DSGInspectExactBacking(ctx context.Context, manager Manager, id string) (DSGExactBackingObservation, error) {
	var b DSGExactBackingObservation
	o, err := DSGInspectExact(ctx, manager, id)
	if err != nil {
		return b, err
	}
	cm := manager.(*cacheManager)
	if cm.Snapshotter == nil || cm.LeaseManager == nil || cm.ContentStore == nil {
		return b, ErrDSGExactPreserve
	}
	_, err = cm.Snapshotter.Stat(ctx, o.SnapshotID)
	if err != nil && !errors.Is(err, errdefs.ErrNotFound) {
		return b, ErrDSGExactPreserve
	}
	b.SnapshotPresent = err == nil
	refs, err := cm.LeaseManager.ListResources(ctx, leases.Lease{ID: id})
	if err != nil && !errors.Is(err, errdefs.ErrNotFound) {
		return DSGExactBackingObservation{}, ErrDSGExactPreserve
	}
	b.RecordLeasePresent = err == nil
	b.LeaseResourceCount = len(refs)
	if o.Blob != "" {
		d, err := digest.Parse(o.Blob)
		if err != nil {
			return DSGExactBackingObservation{}, ErrDSGExactPreserve
		}
		_, err = cm.ContentStore.Info(ctx, d)
		if err != nil && !errors.Is(err, errdefs.ErrNotFound) {
			return DSGExactBackingObservation{}, ErrDSGExactPreserve
		}
		b.BlobPresent = err == nil
	}
	return b, nil
}
