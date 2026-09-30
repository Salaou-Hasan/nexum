; Windows thread shims for `parallel:`.
;
; Included instead of runtime_threads_unix.ll on MSVC targets. The pool
; logic itself lives in the main prelude; only these two OS bindings are
; platform-specific, which keeps the task-claiming code identical
; everywhere and therefore equally well tested.

declare ptr @CreateThread(ptr, i64, ptr, ptr, i32, ptr)
declare i32 @WaitForSingleObject(ptr, i32)
declare i32 @CloseHandle(ptr)

; The HANDLE is CreateThread's return value. Its last argument is
; lpThreadId, a DWORD id, and waiting on that fails every time.
define ptr @nx_thread_start(ptr %fn, ptr %arg) {
entry:
  %tid = alloca i32
  store i32 0, ptr %tid
  %h = call ptr @CreateThread(ptr null, i64 0, ptr %fn, ptr %arg, i32 0, ptr %tid)
  ret ptr %h
}

; INFINITE (-1): wait however long the task takes.
define void @nx_thread_join(ptr %h) {
entry:
  %w = call i32 @WaitForSingleObject(ptr %h, i32 -1)
  call i32 @CloseHandle(ptr %h)
  ret void
}
