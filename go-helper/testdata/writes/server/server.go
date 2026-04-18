// Package server provides a server that writes to cross-package globals.
package server

import "example.com/writes-fixture/registry"

// StartServer initializes the handler registry from a different package.
// This write should appear as a "writes" edge.
func StartServer() {
	registry.HandlerRegistry = map[string]func(){
		"default": func() {},
	}
	registry.DefaultConfig = "production"
}

// SetRetries writes to an unexported var in another package — not valid Go,
// so we test only exported vars here.

// initRegistry is called from init() below.
func initRegistry() {
	registry.GroupedVarA = "initialized"
	registry.GroupedVarB = 42
}

// init runs automatically; writes from init should be captured.
func init() {
	initRegistry()
}
