package wxime

// Windows does not support syncing a directory with os.File.Sync.
func syncDir(path string) error { return nil }
