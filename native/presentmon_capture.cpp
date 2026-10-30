#include "windows_compat.hpp"
#include "PresentMonTraceConsumer.hpp"
#include "PresentMonTraceSession.hpp"
#include <atomic>
#include <chrono>
#include <limits>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <thread>

struct RapoFrame {
    uint64_t timestamp;
    uint64_t swap_chain;
    uint32_t lost;
    uint32_t reserved;
};
static_assert(sizeof(RapoFrame) == 24);

namespace {
using ProcessHandle = std::unique_ptr<void, decltype(&CloseHandle)>;
std::timed_mutex captureMutex;

ULONG processStatus(HANDLE process) {
    switch (WaitForSingleObject(process, 0)) {
    case WAIT_TIMEOUT: return ERROR_SUCCESS;
    case WAIT_OBJECT_0: return ERROR_PROCESS_ABORTED;
    default: return GetLastError();
    }
}

bool creationTime(HANDLE process, uint64_t& value) {
    FILETIME created{}, exited{}, kernel{}, user{};
    if (!GetProcessTimes(process, &created, &exited, &kernel, &user)) return false;
    value = (uint64_t(created.dwHighDateTime) << 32) | created.dwLowDateTime;
    return true;
}

GUID sessionGuid(uint32_t pid, uint64_t created) {
    GUID guid{0x4f504152, static_cast<WORD>(pid), static_cast<WORD>(pid >> 16), {}};
    for (unsigned i = 0; i != 8; ++i) guid.Data4[i] = static_cast<BYTE>(created >> (i * 8));
    return guid;
}

bool number(wchar_t const*& text, uint64_t& value) {
    if (*text < L'0' || *text > L'9') return false;
    value = 0;
    do {
        const auto digit = static_cast<unsigned>(*text - L'0');
        if (value > ((std::numeric_limits<uint64_t>::max)() - digit) / 10) return false;
        value = value * 10 + digit;
        ++text;
    } while (*text >= L'0' && *text <= L'9');
    return true;
}

bool controllerExited(uint32_t pid, uint64_t expectedCreation) {
    ProcessHandle process(OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid), CloseHandle);
    if (!process) return GetLastError() == ERROR_INVALID_PARAMETER;
    const auto wait = WaitForSingleObject(process.get(), 0);
    if (wait == WAIT_OBJECT_0) return true;
    uint64_t created = 0;
    // A reused PID does not identify the controller that created this session.
    return wait == WAIT_TIMEOUT && creationTime(process.get(), created) && created != expectedCreation;
}

ULONG recoverOrphans(bool (*cancelled)(void const*), void const* context) {
    struct Properties {
        EVENT_TRACE_PROPERTIES trace;
        wchar_t name[1025];
        wchar_t file[1025];
    };
    std::vector<Properties> storage(64);
    std::vector<EVENT_TRACE_PROPERTIES*> entries;
    ULONG count = 0;
    for (;;) {
        if (cancelled(context)) return ERROR_CANCELLED;
        entries.resize(storage.size());
        for (size_t i = 0; i != storage.size(); ++i) {
            storage[i] = {};
            storage[i].trace.Wnode.BufferSize = sizeof(Properties);
            storage[i].trace.LoggerNameOffset = offsetof(Properties, name);
            storage[i].trace.LogFileNameOffset = offsetof(Properties, file);
            entries[i] = &storage[i].trace;
        }
        const auto status = QueryAllTracesW(entries.data(), static_cast<ULONG>(entries.size()), &count);
        if (status == ERROR_SUCCESS) break;
        if (status != ERROR_MORE_DATA) return status;
        storage.resize((std::max)(storage.size() * 2, static_cast<size_t>(count)));
    }
    for (ULONG i = 0; i != count; ++i) {
        if (cancelled(context)) return ERROR_CANCELLED;
        const std::wstring_view name(storage[i].name);
        constexpr std::wstring_view prefix = L"RintelRAPO-v1-";
        if (name.substr(0, prefix.size()) != prefix) continue;
        const auto* text = name.data() + prefix.size();
        uint64_t pid = 0, created = 0;
        if (!number(text, pid) || *text++ != L'-' || !number(text, created) || *text != L'\0' ||
            pid == 0 || pid > (std::numeric_limits<uint32_t>::max)() || created == 0) continue;
        const auto guid = sessionGuid(static_cast<uint32_t>(pid), created);
        if (!IsEqualGUID(storage[i].trace.Wnode.Guid, guid) ||
            storage[i].trace.LogFileMode != EVENT_TRACE_REAL_TIME_MODE ||
            !controllerExited(static_cast<uint32_t>(pid), created)) continue;
        // Names alone cannot establish ownership of legacy/third-party sessions.
        Properties properties{};
        properties.trace.Wnode.BufferSize = sizeof(Properties);
        properties.trace.Wnode.Guid = guid;
        properties.trace.LoggerNameOffset = offsetof(Properties, name);
        properties.trace.LogFileNameOffset = offsetof(Properties, file);
        const auto status = ControlTraceW(0, name.data(), &properties.trace, EVENT_TRACE_CONTROL_STOP);
        if (status != ERROR_SUCCESS && status != ERROR_WMI_INSTANCE_NOT_FOUND &&
            status != ERROR_MORE_DATA && status != ERROR_ACTIVE_CONNECTIONS) return status;
    }
    return ERROR_SUCCESS;
}

struct Capture {
    std::unique_lock<std::timed_mutex> ownership;
    ProcessHandle game;
    PMTraceConsumer consumer;
    PMTraceSession session;
    std::thread worker;
    std::atomic<ULONG> status{ERROR_IO_PENDING};
    std::vector<std::shared_ptr<PresentEvent>> presents;
    std::vector<ProcessEvent> processes;
    uint32_t pid;
    ULONG stopStatus = ERROR_IO_PENDING;
    bool finished = false;

    Capture(uint32_t id, ProcessHandle process, std::unique_lock<std::timed_mutex> lock)
        : ownership(std::move(lock)), game(std::move(process)), pid(id) {
        session.mPMConsumer = &consumer;
        // Only application present timing is consumed; avoid display/input/GPU tracking.
        consumer.mTrackDisplay = false;
        consumer.mFilteredProcessIds = true;
        consumer.AddTrackedProcessForFiltering(pid);
    }
    void finish() {
        if (!finished) {
            stopStatus = session.Stop();
            if (worker.joinable()) worker.join();
            finished = true;
        }
    }
    ~Capture() {
        finish();
        if (session.mSessionHandle != 0) session.Stop();
    }
};
}

extern "C" void* rapo_capture_start(uint32_t pid, uint64_t* frequency, uint64_t* origin, uint32_t* error,
                                    bool (*cancelled)(void const*), void const* context) noexcept {
    *error = ERROR_GEN_FAILURE;
    try {
        ProcessHandle game(OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid), CloseHandle);
        if (!game) { *error = GetLastError(); return nullptr; }
        struct Preparation {
            HANDLE game;
            bool (*cancelled)(void const*);
            void const* context;
        } preparation{game.get(), cancelled, context};
        const auto interrupted = +[](void const* pointer) {
            const auto& preparation = *static_cast<Preparation const*>(pointer);
            return preparation.cancelled(preparation.context) || processStatus(preparation.game) != ERROR_SUCCESS;
        };
        std::unique_lock<std::timed_mutex> lock(captureMutex, std::defer_lock);
        while (!lock.try_lock_for(std::chrono::milliseconds(20))) {
            if (interrupted(&preparation)) { *error = ERROR_CANCELLED; return nullptr; }
        }
        if (interrupted(&preparation)) { *error = ERROR_CANCELLED; return nullptr; }
        *error = recoverOrphans(interrupted, &preparation);
        if (*error != ERROR_SUCCESS) return nullptr;
        uint64_t created = 0;
        if (!creationTime(GetCurrentProcess(), created)) { *error = GetLastError(); return nullptr; }
        // One stable name per controller instance prevents retries consuming logger slots.
        const auto name = L"RintelRAPO-v1-" + std::to_wstring(GetCurrentProcessId()) + L"-" + std::to_wstring(created);
        const auto guid = sessionGuid(GetCurrentProcessId(), created);
        auto capture = std::make_unique<Capture>(pid, std::move(game), std::move(lock));
        if (!capture->consumer.hEventsReadyEvent) { *error = GetLastError(); return nullptr; }
        *error = capture->session.Start(nullptr, name.c_str(), &guid, interrupted, &preparation);
        if (*error != ERROR_SUCCESS) return nullptr;
        if (interrupted(&preparation)) { *error = ERROR_CANCELLED; return nullptr; }
        if (capture->session.mTimestampType != PMTraceSession::TIMESTAMP_TYPE_QPC || capture->session.mTimestampFrequency.QuadPart <= 0) {
            *error = ERROR_INVALID_DATA;
            return nullptr;
        }
        *frequency = static_cast<uint64_t>(capture->session.mTimestampFrequency.QuadPart);
        *origin = static_cast<uint64_t>(capture->session.mStartTimestamp.QuadPart);
        const auto trace = capture->session.mTraceHandle;
        auto* state = capture.get();
        capture->worker = std::thread([state, trace]() {
            auto handle = trace;
            ULONG status;
            try { status = ProcessTrace(&handle, 1, nullptr, nullptr); }
            catch (...) { status = ERROR_INVALID_DATA; }
            state->status.store(status, std::memory_order_release);
        });
        *error = ERROR_SUCCESS;
        return capture.release();
    } catch (const std::bad_alloc&) { *error = ERROR_NOT_ENOUGH_MEMORY; }
      catch (...) { *error = ERROR_GEN_FAILURE; }
    return nullptr;
}

extern "C" uint32_t rapo_capture_read(void* handle, RapoFrame* frames, uint32_t capacity, uint32_t* count) noexcept {
    auto& capture = *static_cast<Capture*>(handle);
    *count = 0;
    try {
        const auto gameStatus = processStatus(capture.game.get());
        if (gameStatus != ERROR_SUCCESS) return gameStatus;
        const auto status = capture.status.load(std::memory_order_acquire);
        if (!capture.finished && status != ERROR_IO_PENDING) return status == ERROR_SUCCESS ? ERROR_OPERATION_ABORTED : status;
        capture.processes.clear();
        capture.consumer.DequeueProcessEvents(capture.processes);
        capture.consumer.DequeuePresentEvents(capture.presents);
        for (const auto& present : capture.presents) {
            if (present->ProcessId != capture.pid || present->PresentFailed) continue;
            if (*count == capacity) return ERROR_MORE_DATA;
            frames[(*count)++] = {present->PresentStartTime, present->SwapChainAddress, present->IsLost ? 1u : 0u, 0};
        }
        return processStatus(capture.game.get());
    } catch (const std::bad_alloc&) { return ERROR_NOT_ENOUGH_MEMORY; }
      catch (...) { return ERROR_INVALID_DATA; }
}

extern "C" uint32_t rapo_capture_finish(void* handle, uint32_t* lost) noexcept {
    auto& capture = *static_cast<Capture*>(handle);
    *lost = 1;
    try {
        capture.finish();
        if (capture.stopStatus != ERROR_SUCCESS) return capture.stopStatus;
        const auto gameStatus = processStatus(capture.game.get());
        if (gameStatus != ERROR_SUCCESS) return gameStatus;
        *lost = (capture.session.mNumEventsLost != 0 || capture.session.mNumBuffersLost != 0 || capture.consumer.mCompletedOverflow) ? 1u : 0u;
        const auto status = capture.status.load(std::memory_order_acquire);
        return status == ERROR_SUCCESS || status == ERROR_CANCELLED ? ERROR_SUCCESS : status;
    } catch (...) { return ERROR_GEN_FAILURE; }
}

extern "C" void rapo_capture_stop(void* handle) noexcept {
    delete static_cast<Capture*>(handle);
}
