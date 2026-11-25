package cache

import "context"

// These roles share one kernel lock on the dedicated state-directory inode.
// No authority over a stock daemon or remote containerd follows from this lock.
func DSGWithDedicatedWorker(root string, run func() error) error {
	return dsgWithWorkerRoot(root, run)
}

func DSGWithDedicatedMaintenance(root string, run func() error) error {
	return dsgWithWorkerRoot(root, run)
}

// The inspection capability is private. The dedicated daemon integration does
// not install one for native stores: their cross-store effects are unproven.
type dsgDedicatedFence struct {
	root    string
	inspect func(context.Context) (dsgFrozenInventory, error)
}

func (f *dsgDedicatedFence) withFrozenInventory(ctx context.Context, apply func(dsgFrozenInventory) error) error {
	if f == nil || f.inspect == nil || apply == nil || ctx.Err() != nil {
		return ErrDSGExactPreserve
	}
	return DSGWithDedicatedMaintenance(f.root, func() error {
		inventory, err := f.inspect(ctx)
		if err != nil {
			return err
		}
		return apply(inventory)
	})
}
