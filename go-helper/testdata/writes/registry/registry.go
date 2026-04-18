// Package registry holds package-level variables that other packages write to.
package registry

// HandlerRegistry is a cross-package writable global (exported var).
var HandlerRegistry map[string]func()

// DefaultConfig holds configuration set during initialization.
var DefaultConfig string

// maxRetries is unexported but cross-package writes should still be detected.
var maxRetries int

// GroupedVarA and GroupedVarB are in a grouped var declaration.
var (
	GroupedVarA string
	GroupedVarB int
)

// MaxBatchSize is a package-level const.
const MaxBatchSize = 100

// samePackageWriter writes to HandlerRegistry from within the same package.
// This write should NOT appear in helper output (filter-at-source).
func samePackageWriter() {
	HandlerRegistry = make(map[string]func())
}
