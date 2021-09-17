//go:build tools
// +build tools

package main

// This is a list of commands the project depends on. Listing them here allows
// for versions to be managed in go.mod and `go install` to pull those versions.
import (
	_ "sigs.k8s.io/controller-tools/cmd/controller-gen"
	_ "sigs.k8s.io/kustomize/kustomize/v4"
)
