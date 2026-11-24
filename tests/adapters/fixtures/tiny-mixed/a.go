package tiny

type Doer interface{ Do() int }

type impl struct{}

func (impl) Do() int { return 1 }

func Use(d Doer) int { return d.Do() }
