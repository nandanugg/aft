package app

import (
	"strings"

	"golang.org/x/tools/go/ast/astutil"
)

func Use() {
	_ = strings.ToUpper("a")
	_ = astutil.Unparen(nil)
}
