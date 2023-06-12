package main

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/quay/clair/config"
)

import "C"

// Validate runs [config.Validate] on the config.
//
// Lints and warning are copied into a C allocation and the "out" pointer
// filled, if provided. If the Go `Validate` function returned an error, the
// return will be non-zero. This function fills in the "Mode" member of the
// target struct, which the Go documentation assumes will be done.
//
// [config.Validate]: https://pkg.go.dev/github.com/quay/clair/config#Validate
//
//export Validate
func Validate(b []byte, out **C.char, mode string) (exit C.int) {
	var buf strings.Builder
	var cfg config.Config
	var err error
	defer func() {
		if err != nil {
			buf.Reset()
			buf.WriteString(err.Error())
		} else {
			exit = 0
		}
		*out = C.CString(buf.String())
	}()

	exit++
	cfg.Mode, err = config.ParseMode(mode)
	if err != nil {
		return
	}

	exit++
	cfg.Mode, err = config.ParseMode(mode)
	err = json.Unmarshal(b, &cfg)
	if err != nil {
		return
	}

	var ws []config.Warning
	exit++
	cfg.Mode, err = config.ParseMode(mode)
	ws, err = config.Validate(&cfg)
	for _, w := range ws {
		fmt.Fprintln(&buf, w.Error())
	}
	return
}

func main() {
	panic("not a real main -- build as c-archive")
}
