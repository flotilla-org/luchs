// CEF owns renderer/GPU child process dispatch and sandbox initialization.
#include "include/cef_app.h"
#include "include/cef_sandbox_mac.h"
#include "include/wrapper/cef_library_loader.h"
int main(int argc, char **argv) {
  CefScopedSandboxContext sandbox;
  if (!sandbox.Initialize(argc, argv))
    return 1;
  CefScopedLibraryLoader library;
  if (!library.LoadInHelper())
    return 1;
  CefMainArgs args(argc, argv);
  return CefExecuteProcess(args, nullptr, nullptr);
}
