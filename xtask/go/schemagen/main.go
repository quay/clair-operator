package main

import (
	"encoding/json"
	"flag"
	"os"
	"reflect"

	"github.com/invopop/jsonschema"
	"github.com/quay/clair/config"
)

func main() {
	flag.Parse()

	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "\t")
	s := jsonschema.ReflectFromType(reflect.TypeOf(config.Config{}))
	if err := enc.Encode(s); err != nil {
		panic(err)
	}
}
