package controllers

import (
	"io/fs"

	"sigs.k8s.io/kustomize/api/filesys"
)

// This copies everything out of the embedded files, because it's simpler than
// attempting an adapter struct.

var templatesFS = filesys.MakeFsInMemory()

func init() {
	sub, err := fs.Sub(templates, "templates")
	if err != nil {
		panic(err)
	}
	err = fs.WalkDir(sub, ".", func(n string, d fs.DirEntry, err error) error {
		if d.IsDir() {
			return templatesFS.Mkdir(n)
		}
		b, err := fs.ReadFile(sub, n)
		if err != nil {
			return err
		}
		if err := templatesFS.WriteFile(n, b); err != nil {
			return err
		}
		return nil
	})
	if err != nil {
		panic(err)
	}
}
