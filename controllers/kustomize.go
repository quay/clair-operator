package controllers

import (
	"errors"
	"fmt"
	"io/fs"

	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"sigs.k8s.io/kustomize/api/filesys"
	"sigs.k8s.io/kustomize/api/krusty"
	"sigs.k8s.io/kustomize/api/resid"
	"sigs.k8s.io/kustomize/api/resmap"
	"sigs.k8s.io/kustomize/api/resource"
	"sigs.k8s.io/kustomize/kyaml/kio"
	kyaml "sigs.k8s.io/kustomize/kyaml/yaml"
)

// Kustomize is a helper around sigs.k8s.io/kustomize/api/krusty.
//
// It makes the Kustomizer talk to embedded templates. This should be much
// simpler once sigs.k8s.io/kustomize/api/filesys adopts the go1.16 "fs" APIs.
type kustomize struct {
	*krusty.Kustomizer
	fs filesys.FileSystem
}

// NewKustomize creates a kustomize and associates all the embedded packages with it.
func newKustomize() (*kustomize, error) {
	tfs := filesys.MakeFsInMemory()
	sub, err := fs.Sub(templates, "templates")
	if err != nil {
		return nil, err
	}
	err = fs.WalkDir(sub, ".", func(n string, d fs.DirEntry, err error) error {
		if d.IsDir() {
			return tfs.Mkdir(n)
		}
		if err != nil {
			return err
		}
		b, err := fs.ReadFile(sub, n)
		if err != nil {
			return err
		}
		if err := tfs.WriteFile(n, b); err != nil {
			return err
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	opts := krusty.MakeDefaultOptions()
	k := krusty.MakeKustomizer(opts)
	return &kustomize{
		Kustomizer: k,
		fs:         tfs,
	}, nil
}

func findDeployment(r resid.ResId) bool {
	// apps	v1	Deployment
	test := &resid.Gvk{
		Group:   "apps",
		Version: "v1",
		Kind:    "Deployment",
	}
	return r.IsSelected(test)
}

func (k *kustomize) Run(cfg *unstructured.Unstructured, which string, image string) (resmap.ResMap, error) {
	if image == "" {
		return nil, errors.New("kustomize: no image provided")
	}
	res, err := k.Kustomizer.Run(k.fs, which)
	if err != nil {
		return nil, fmt.Errorf("kustomize: run error: %w", err)
	}

	var configSetter kyaml.Filter
	switch cfg.GroupVersionKind().Kind {
	case "Secret":
		m, err := kyaml.FromMap(map[string]interface{}{
			"secretName": cfg.GetName(),
			"optional":   false,
		})
		if err != nil {
			return nil, err
		}
		configSetter = kyaml.SetField("secret", m)
	case "ConfigMap":
		m, err := kyaml.FromMap(map[string]interface{}{
			"name":     cfg.GetName(),
			"optional": false,
		})
		if err != nil {
			return nil, err
		}
		configSetter = kyaml.SetField("configMap", m)
	default:
		panic("programmer error")
	}

	rs := res.GetMatchingResourcesByCurrentId(findDeployment)
	if len(rs) == 0 {
		return nil, errors.New("unable to find deployments")
	}
	imageSetter := kyaml.SetField("image", kyaml.NewStringRNode(image))

	var d *resource.Resource
	for _, r := range rs {
		if n, ok := r.GetLabels()["app.kubernetes.io/name"]; !ok || n != "clair" {
			continue
		}
		d = r
	}
	if d == nil {
		return nil, errors.New("unable to find clair deployment")
	}
	if err := d.ApplyFilter(kio.FilterFunc(func(ns []*kyaml.RNode) ([]*kyaml.RNode, error) {
		for _, n := range ns {
			if err := n.PipeE(
				kyaml.Lookup("spec", "template", "spec", "volumes", "[name=config]"),
				configSetter,
			); err != nil {
				return nil, err
			}
		}
		return ns, nil
	})); err != nil {
		return nil, fmt.Errorf("kustomize: pipeline error: %w", err)
	}
	if err := d.ApplyFilter(kio.FilterFunc(func(ns []*kyaml.RNode) ([]*kyaml.RNode, error) {
		for _, n := range ns {
			if err := n.PipeE(
				kyaml.Lookup("spec", "template", "spec", "containers", "[name=clair]"),
				imageSetter,
			); err != nil {
				return nil, err
			}
		}
		return ns, nil
	})); err != nil {
		return nil, fmt.Errorf("kustomize: pipeline error: %w", err)
	}

	if _, err := res.Replace(d); err != nil {
		return nil, fmt.Errorf("kustomize: node replace error: %w", err)
	}
	return res, nil
}

func (k *kustomize) Database(image string) (resmap.ResMap, error) {
	if image == "" {
		return nil, errors.New("kustomize: no image provided")
	}
	res, err := k.Kustomizer.Run(k.fs, "database")
	if err != nil {
		return nil, fmt.Errorf("kustomize: database error: %w", err)
	}
	_ = res
	return nil, nil
}
