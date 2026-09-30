; unix thread shims for `parallel:`.
;
; Included instead of runtime_threads_win.ll on unix targets. The pool
; logic itself lives in the main prelude; only these two OS bindings are
; platform-specific, which keeps the task-claiming code identical
; everywhere and therefore equally well tested.

declare i32 @pthread_create(ptr, ptr, ptr, ptr)
declare i32 @pthread_join(i64, ptr)

; pthread_t travels as i64 (it is a pointer on some platforms and a
; 32-bit id on others), so the handle is returned as an integer.
define i64 @nx_thread_start(ptr %fn, ptr %arg) {
entry:
  %tid = alloca i64
  store i64 0, ptr %tid
  %rc = call i32 @pthread_create(ptr %tid, ptr null, ptr %fn, ptr %arg)
  %h = load i64, ptr %tid
  ret i64 %h
}

define void @nx_thread_join(i64 %h) {
entry:
  %rc = call i32 @pthread_join(i64 %h, ptr null)
  ret void
}
