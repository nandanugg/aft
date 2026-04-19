// sample_vars.go — fixture for package-level var/const extraction.
package sample

// SingleVar is a single package-level var declaration.
var SingleVar = "hello"

// ExportedConst is a single package-level const.
const ExportedConst = 42

// unexportedVar is a private var.
var unexportedVar int

// Grouped var declaration — each name should emit a separate symbol.
var (
	GroupedA string
	GroupedB int
	GroupedC float64
)

// Grouped const declaration.
const (
	ConstX = 1
	ConstY = 2
)

// PackageFunc is a function (should still appear as Function, not Variable).
func PackageFunc() {}
