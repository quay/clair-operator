package controllers

import (
	"errors"
	"fmt"

	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"sigs.k8s.io/kustomize/api/krusty"
	"sigs.k8s.io/kustomize/api/resid"
	"sigs.k8s.io/kustomize/api/resmap"
	"sigs.k8s.io/kustomize/api/resource"
	"sigs.k8s.io/kustomize/kyaml/kio"
	kyaml "sigs.k8s.io/kustomize/kyaml/yaml"
)

type kustomize struct {
	*krusty.Kustomizer
}

func newKustomize() *kustomize {
	opts := krusty.MakeDefaultOptions()
	k := krusty.MakeKustomizer(opts)
	return &kustomize{k}
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

func (k *kustomize) run(cfg *unstructured.Unstructured, which string) (resmap.ResMap, error) {
	res, err := k.Kustomizer.Run(templatesFS, which)
	if err != nil {
		return nil, fmt.Errorf("kustomize: run error: %w", err)
	}

	var setter kyaml.Filter
	switch cfg.GroupVersionKind().Kind {
	case "Secret":
		m, err := kyaml.FromMap(map[string]interface{}{
			"secretName": cfg.GetName(),
			"optional":   false,
		})
		if err != nil {
			return nil, err
		}
		setter = kyaml.SetField("secret", m)
	case "ConfigMap":
		m, err := kyaml.FromMap(map[string]interface{}{
			"name":     cfg.GetName(),
			"optional": false,
		})
		if err != nil {
			return nil, err
		}
		setter = kyaml.SetField("configMap", m)
	default:
		panic("programmer error")
	}
	if err != nil {
		return nil, fmt.Errorf("kustomize: node creation error: %w", err)
	}
	rs := res.GetMatchingResourcesByCurrentId(findDeployment)
	if len(rs) == 0 {
		return nil, errors.New("unable to find deployments")
	}

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
				setter,
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

func (k *kustomize) Indexer(cfg configObject) (resmap.ResMap, error) {
	return k.run(cfg, "indexer")
}

func (k *kustomize) Matcher(cfg configObject) (resmap.ResMap, error) {
	return k.run(cfg, "matcher")
}

func (k *kustomize) Notifier(cfg configObject) (resmap.ResMap, error) {
	return k.run(cfg, "notifier")
}
