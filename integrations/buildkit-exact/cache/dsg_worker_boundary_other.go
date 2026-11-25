//go:build !linux && !darwin

package cache

func dsgWithWorkerRoot(root string, run func() error) error { return ErrDSGExactPreserve }
