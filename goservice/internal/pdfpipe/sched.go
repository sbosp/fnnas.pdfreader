package pdfpipe

import (
	"container/heap"
	"context"
	"sync"
)

// 同书渲页串行，但按 pri 插队（越小越优先）。客户端取消时从队列摘掉，不占文档锁。

type slotWaiter struct {
	pri int
	seq uint64
	ctx context.Context
	ch  chan struct{}
	idx int
}

type waiterHeap []*slotWaiter

func (h waiterHeap) Len() int { return len(h) }
func (h waiterHeap) Less(i, j int) bool {
	if h[i].pri != h[j].pri {
		return h[i].pri < h[j].pri
	}
	return h[i].seq < h[j].seq
}
func (h waiterHeap) Swap(i, j int) {
	h[i], h[j] = h[j], h[i]
	h[i].idx = i
	h[j].idx = j
}
func (h *waiterHeap) Push(x any) {
	w := x.(*slotWaiter)
	w.idx = len(*h)
	*h = append(*h, w)
}
func (h *waiterHeap) Pop() any {
	old := *h
	n := len(old)
	w := old[n-1]
	old[n-1] = nil
	w.idx = -1
	*h = old[:n-1]
	return w
}

type docSlot struct {
	mu     sync.Mutex
	heap   waiterHeap
	seq    uint64
	active bool
}

type slotTable struct {
	mu    sync.Mutex
	slots map[string]*docSlot
}

func (t *slotTable) get(path string) *docSlot {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.slots == nil {
		t.slots = make(map[string]*docSlot)
	}
	s, ok := t.slots[path]
	if !ok {
		s = &docSlot{}
		heap.Init(&s.heap)
		t.slots[path] = s
	}
	return s
}

func (s *docSlot) acquire(ctx context.Context, pri int) error {
	if pri < 0 {
		pri = 0
	}
	s.mu.Lock()
	if !s.active {
		s.active = true
		s.mu.Unlock()
		if err := ctx.Err(); err != nil {
			s.release()
			return err
		}
		return nil
	}
	w := &slotWaiter{pri: pri, ctx: ctx, ch: make(chan struct{})}
	s.seq++
	w.seq = s.seq
	heap.Push(&s.heap, w)
	s.mu.Unlock()

	select {
	case <-w.ch:
		if err := ctx.Err(); err != nil {
			s.release()
			return err
		}
		return nil
	case <-ctx.Done():
		s.mu.Lock()
		if w.idx >= 0 && w.idx < len(s.heap) && s.heap[w.idx] == w {
			heap.Remove(&s.heap, w.idx)
		}
		granted := false
		select {
		case <-w.ch:
			granted = true
		default:
		}
		s.mu.Unlock()
		if granted {
			s.release()
		}
		return ctx.Err()
	}
}

func (s *docSlot) release() {
	s.mu.Lock()
	defer s.mu.Unlock()
	for len(s.heap) > 0 {
		w := heap.Pop(&s.heap).(*slotWaiter)
		if w.ctx.Err() != nil {
			continue
		}
		close(w.ch)
		return
	}
	s.active = false
}
