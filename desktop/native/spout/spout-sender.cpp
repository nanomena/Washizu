// A Spout sender for the game's frames, run by the shell (src/spout.js) on
// Windows. The game renders offscreen; for each frame Electron hands the shell
// a Direct3D 11 texture as an NT handle that is valid in the shell's process.
// This program opens that texture on a device of its own, copies it into the
// texture it shares under Spout's sender name, and says when it is done, so
// that the shell can give the texture back to Chromium. A streaming program
// with a Spout source (OBS: the Spout2 plugin) then reads the shared texture
// directly: no frame is copied through the CPU on either side.
//
//   spout-sender.exe <shell-pid> [--name <sender>]
//
//   stdin   "<id> <nt-handle> <width> <height>"  one line a frame
//   stdout  "<id> ok" | "<id> err <what>"         once the copy has run on the GPU
//   stderr  what it is doing, a line at a time
//
// It ends, and leaves Spout's list of senders, when stdin closes.
//
// Spout's shared memory, mutexes and texture follow the Spout2 SDK
// (https://github.com/leadedge/Spout2, BSD-2-Clause, Lynn Jarvis):
// SpoutSenderNames.cpp, SpoutSharedMemory.cpp, SpoutFrameCount.cpp and
// SpoutDirectX.cpp. Only what a sender of DirectX 11 textures needs is here.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d11_1.h>
#include <dxgi1_2.h>
#include <climits>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iterator>
#include <set>
#include <string>

#pragma comment(lib, "d3d11.lib")
#pragma comment(lib, "dxgi.lib")
#pragma comment(lib, "advapi32.lib")

namespace {

const size_t NAME_LEN = 256;   // SpoutMaxSenderNameLen
const DWORD LOCK_MS = 67;      // the SDK's wait for its mutexes: four frames at 60 fps

// What a receiver reads about a sender, in the shared memory named after it.
#pragma pack(push, 4)
struct SharedTextureInfo {
  uint32_t shareHandle;
  uint32_t width;
  uint32_t height;
  uint32_t format;
  uint32_t usage;
  uint8_t description[256];
  uint32_t partnerId;
};
#pragma pack(pop)
static_assert(sizeof(SharedTextureInfo) == 280, "Spout's SharedTextureInfo is 280 bytes");

void say(const char* format, ...) {
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
  fputc('\n', stderr);
  fflush(stderr);
}

// A named piece of shared memory with the mutex the SDK pairs with it ("<name>_mutex").
struct Shared {
  HANDLE map = nullptr;
  HANDLE mutex = nullptr;
  uint8_t* view = nullptr;
  bool create(const std::string& name, size_t size) {
    map = CreateFileMappingA(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, (DWORD)size, name.c_str());
    if (!map) return false;
    view = (uint8_t*)MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    mutex = CreateMutexA(nullptr, FALSE, (name + "_mutex").c_str());
    return view && mutex;
  }
  bool lock() { return WaitForSingleObject(mutex, LOCK_MS) == WAIT_OBJECT_0; }
  void unlock() { ReleaseMutex(mutex); }
  void close() {
    if (view) UnmapViewOfFile(view);
    if (map) CloseHandle(map);
    if (mutex) CloseHandle(mutex);
    view = nullptr;
    map = mutex = nullptr;
  }
};

DWORD maxSenders() {
  DWORD value = 64, size = sizeof value;
  if (RegGetValueA(HKEY_CURRENT_USER, "Software\\Leading Edge\\Spout", "MaxSenders", RRF_RT_REG_DWORD, nullptr, &value, &size) != ERROR_SUCCESS) value = 64;
  return value ? value : 64;
}

// A sender is still there while its shared memory is: the SDK drops names whose memory has gone.
bool alive(const std::string& name) {
  HANDLE map = OpenFileMappingA(FILE_MAP_READ, FALSE, name.c_str());
  if (!map) return false;
  CloseHandle(map);
  return true;
}

std::set<std::string> readNames(const uint8_t* buf, DWORD max) {
  std::set<std::string> names;
  for (DWORD i = 0; i < max; i++) {
    const char* name = (const char*)buf + i * NAME_LEN;
    if (!name[0]) break;
    names.insert(std::string(name, strnlen(name, NAME_LEN)));
  }
  return names;
}

void writeNames(uint8_t* buf, const std::set<std::string>& names, DWORD max) {
  DWORD i = 0;
  for (const auto& name : names) {
    if (i == max) break;
    strncpy_s((char*)buf + i * NAME_LEN, NAME_LEN, name.c_str(), _TRUNCATE);
    i++;
  }
  if (i < max) buf[i * NAME_LEN] = 0;
}

struct Sender {
  std::string name;
  DWORD max = 64;
  Shared list, active, info;
  HANDLE access = nullptr;
  HANDLE count = nullptr;

  bool open(const std::string& wanted) {
    name = wanted;
    max = maxSenders();
    if (!list.create("SpoutSenderNames", max * NAME_LEN)) return false;
    if (!active.create("ActiveSenderName", NAME_LEN)) return false;
    if (!info.create(name, sizeof(SharedTextureInfo))) return false;
    access = CreateMutexA(nullptr, FALSE, (name + "_SpoutAccessMutex").c_str());
    count = CreateSemaphoreA(nullptr, 1, LONG_MAX, (name + "_Count_Semaphore").c_str());
    if (!access) return false;
    if (!list.lock()) return false;
    std::set<std::string> names = readNames(list.view, max);
    for (auto it = names.begin(); it != names.end();) it = (*it != name && !alive(*it)) ? names.erase(it) : std::next(it);
    names.insert(name);
    writeNames(list.view, names, max);
    list.unlock();
    // The newest sender is the active one, as the SDK registers it.
    if (active.lock()) {
      memset(active.view, 0, NAME_LEN);
      strncpy_s((char*)active.view, NAME_LEN, name.c_str(), _TRUNCATE);
      active.unlock();
    }
    return true;
  }

  void describe(uint32_t handle, uint32_t width, uint32_t height) {
    SharedTextureInfo record = {};
    record.shareHandle = handle;
    record.width = width;
    record.height = height;
    record.format = DXGI_FORMAT_B8G8R8A8_UNORM;
    GetModuleFileNameA(nullptr, (char*)record.description, sizeof record.description - 1);
    if (info.lock()) {
      memcpy(info.view, &record, sizeof record);
      info.unlock();
    }
  }

  // What the SDK's SetNewFrame does: the count goes up by one a frame.
  void counted() {
    if (count && WaitForSingleObject(count, 0) == WAIT_OBJECT_0) ReleaseSemaphore(count, 2, nullptr);
  }

  void close() {
    if (list.view && list.lock()) {
      std::set<std::string> names = readNames(list.view, max);
      names.erase(name);
      writeNames(list.view, names, max);
      list.unlock();
      if (active.view && active.lock()) {
        if (name == (const char*)active.view) {
          memset(active.view, 0, NAME_LEN);
          if (!names.empty()) strncpy_s((char*)active.view, NAME_LEN, names.begin()->c_str(), _TRUNCATE);
        }
        active.unlock();
      }
    }
    info.close();
    active.close();
    list.close();
    if (access) CloseHandle(access);
    if (count) CloseHandle(count);
    access = count = nullptr;
  }
};

struct Device {
  ID3D11Device* dev = nullptr;
  ID3D11Device1* dev1 = nullptr;
  ID3D11DeviceContext* ctx = nullptr;
  ID3D11Query* done = nullptr;
  bool open() {
    // The default adapter: the one Chromium's GPU process renders on unless told otherwise.
    D3D_FEATURE_LEVEL level;
    if (FAILED(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION, &dev, &level, &ctx))) return false;
    if (FAILED(dev->QueryInterface(__uuidof(ID3D11Device1), (void**)&dev1))) return false;
    D3D11_QUERY_DESC desc = {D3D11_QUERY_EVENT, 0};
    return SUCCEEDED(dev->CreateQuery(&desc, &done));
  }
  // Until the GPU has run what is queued: the frame goes back to Chromium after this.
  void finish() {
    ctx->End(done);
    ctx->Flush();
    while (ctx->GetData(done, nullptr, 0, 0) == S_FALSE) YieldProcessor();
  }
};

}  // namespace

int main(int argc, char** argv) {
  if (argc < 2) {
    say("usage: spout-sender <shell-pid> [--name <sender>]");
    return 2;
  }
  DWORD pid = strtoul(argv[1], nullptr, 10);
  std::string name = "Washizu";
  for (int i = 2; i + 1 < argc; i++) if (!strcmp(argv[i], "--name")) name = argv[i + 1];
  if (name.empty() || name.size() >= NAME_LEN) {
    say("sender name must be 1 to 255 characters");
    return 2;
  }

  HANDLE shell = OpenProcess(PROCESS_DUP_HANDLE, FALSE, pid);
  if (!shell) {
    say("cannot open process %lu: %lu", pid, GetLastError());
    return 1;
  }
  Device d;
  if (!d.open()) {
    say("no Direct3D 11 device");
    return 1;
  }
  Sender sender;
  if (!sender.open(name)) {
    say("cannot register the Spout sender \"%s\"", name.c_str());
    sender.close();
    return 1;
  }
  say("sending as \"%s\"", name.c_str());

  ID3D11Texture2D* shared = nullptr;
  UINT width = 0, height = 0;
  char line[256];
  while (fgets(line, sizeof line, stdin)) {
    long long id;
    unsigned long long value;
    unsigned w, h;
    if (sscanf_s(line, "%lld %llu %u %u", &id, &value, &w, &h) != 4) continue;
    HANDLE dup = nullptr;
    if (!DuplicateHandle(shell, (HANDLE)(uintptr_t)value, GetCurrentProcess(), &dup, 0, FALSE, DUPLICATE_SAME_ACCESS)) {
      printf("%lld err duplicate %lu\n", id, GetLastError());
      fflush(stdout);
      continue;
    }
    ID3D11Texture2D* frame = nullptr;
    HRESULT hr = d.dev1->OpenSharedResource1(dup, __uuidof(ID3D11Texture2D), (void**)&frame);
    CloseHandle(dup);
    if (FAILED(hr)) {
      printf("%lld err open %08lx\n", id, (unsigned long)hr);
      fflush(stdout);
      continue;
    }
    if (!shared || width != w || height != h) {
      if (shared) shared->Release();
      shared = nullptr;
      D3D11_TEXTURE2D_DESC desc = {w, h, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT,
                                   D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, D3D11_RESOURCE_MISC_SHARED};
      IDXGIResource* resource = nullptr;
      HANDLE handle = nullptr;
      if (FAILED(d.dev->CreateTexture2D(&desc, nullptr, &shared)) || FAILED(shared->QueryInterface(__uuidof(IDXGIResource), (void**)&resource)) ||
          FAILED(resource->GetSharedHandle(&handle))) {
        if (resource) resource->Release();
        frame->Release();
        printf("%lld err texture\n", id);
        fflush(stdout);
        continue;
      }
      resource->Release();
      width = w;
      height = h;
      sender.describe((uint32_t)(uintptr_t)handle, w, h);
      say("sharing %ux%u", w, h);
    }
    // Under the sender's access mutex, as receivers expect; a receiver that
    // holds it too long costs this frame, not the game.
    if (WaitForSingleObject(sender.access, LOCK_MS) == WAIT_OBJECT_0) {
      d.ctx->CopyResource(shared, frame);
      d.finish();
      sender.counted();
      ReleaseMutex(sender.access);
      printf("%lld ok\n", id);
    } else {
      d.finish();
      printf("%lld err busy\n", id);
    }
    fflush(stdout);
    frame->Release();
  }

  sender.close();
  if (shared) shared->Release();
  say("stopped");
  return 0;
}
