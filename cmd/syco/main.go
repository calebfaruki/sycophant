package main

import (
	"os"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/cmd"
)

func main() {
	if cmd.Root().Execute() != nil {
		os.Exit(1)
	}
}
