package controllers

import "os"

// These constants are the environment variables used for images.
const (
	EnvPostgresImage = `RELATED_IMAGE_POSTGRES`
	EnvClairImage    = `RELATED_IMAGE_CLAIR`
)

var postgresImage = os.Getenv(EnvPostgresImage)

// ClairImage is the default image used for creating Deployments.
//
// Populated by the RELATED_IMAGE_CLAIR environment variable, or set to the
// default "quay.io/projectquay/clair:latest".
var clairImage string = os.Getenv("RELATED_IMAGE_CLAIR")

func init() {
	if clairImage == "" {
		clairImage = `quay.io/projectquay/clair:latest`
	}
}
