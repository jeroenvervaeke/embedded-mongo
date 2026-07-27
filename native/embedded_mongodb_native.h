#pragma once

#include <cstddef>
#include <cstdint>

#if defined(__GNUC__)
#define EMBEDDED_MONGODB_API __attribute__((visibility("default")))
#else
#define EMBEDDED_MONGODB_API
#endif

extern "C" {

struct embedded_mongodb_handle;

struct embedded_mongodb_buffer {
    std::uint8_t* data;
    std::size_t len;
};

EMBEDDED_MONGODB_API int embedded_mongodb_initialize(char** error) noexcept;

EMBEDDED_MONGODB_API int embedded_mongodb_open(const char* path,
                                                std::size_t path_len,
                                                embedded_mongodb_handle** handle,
                                                char** error) noexcept;

EMBEDDED_MONGODB_API int embedded_mongodb_run_command(embedded_mongodb_handle* handle,
                                                       const char* database,
                                                       std::size_t database_len,
                                                       const std::uint8_t* command,
                                                       std::size_t command_len,
                                                       embedded_mongodb_buffer* response,
                                                       char** error) noexcept;

EMBEDDED_MONGODB_API int embedded_mongodb_close(embedded_mongodb_handle* handle,
                                                 char** error) noexcept;

EMBEDDED_MONGODB_API void embedded_mongodb_free(void* memory) noexcept;

}
