package controllers

import "embed"

//go:embed templates

// Templates is the embedded kustomize files.
var templates embed.FS
