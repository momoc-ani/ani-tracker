#include "rife.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstring>
#include <filesystem>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <string>
#include <vector>

#if defined(_WIN32)
#include <fcntl.h>
#include <io.h>
#endif

#include "gpu.h"

namespace fs = std::filesystem;

namespace {

constexpr std::array<std::uint8_t, 8> kMagic{'A', 'N', 'I', 'F', 'R', 'M', '1', '\0'};
constexpr std::uint16_t kProtocolVersion = 1;
constexpr std::size_t kHeaderBytes = 48;
constexpr std::size_t kMaxPayloadBytes = 128ULL * 1024ULL * 1024ULL;

enum class MessageKind : std::uint16_t {
  kHandshakeRequest = 1,
  kHandshakeResponse = 2,
  kWarmupRequest = 3,
  kInterpolateRequest = 4,
  kFrameResponse = 5,
  kErrorResponse = 6,
  kShutdownRequest = 7,
};

struct Message {
  MessageKind kind{};
  std::uint64_t request_id = 0;
  std::uint32_t width = 0;
  std::uint32_t height = 0;
  std::uint32_t stride = 0;
  std::int64_t pts_micros = 0;
  std::vector<std::uint8_t> payload;
};

struct Options {
  fs::path model_directory;
  std::string model_id;
};

template <typename T>
T read_little_endian(const std::uint8_t* data) {
  T value = 0;
  for (std::size_t index = 0; index < sizeof(T); ++index) {
    value |= static_cast<T>(data[index]) << (index * 8);
  }
  return value;
}

template <typename T>
void write_little_endian(std::uint8_t* data, T value) {
  for (std::size_t index = 0; index < sizeof(T); ++index) {
    data[index] = static_cast<std::uint8_t>((value >> (index * 8)) & 0xff);
  }
}

bool read_exact(std::istream& input, void* destination, std::size_t length) {
  input.read(static_cast<char*>(destination), static_cast<std::streamsize>(length));
  if (input.gcount() == 0 && input.eof()) return false;
  if (input.gcount() != static_cast<std::streamsize>(length)) {
    throw std::runtime_error("truncated sidecar request");
  }
  return true;
}

Message read_message(std::istream& input) {
  std::array<std::uint8_t, kHeaderBytes> header{};
  if (!read_exact(input, header.data(), header.size())) {
    throw std::runtime_error("sidecar input closed");
  }
  if (!std::equal(kMagic.begin(), kMagic.end(), header.begin()) ||
      read_little_endian<std::uint16_t>(&header[8]) != kProtocolVersion) {
    throw std::runtime_error("unsupported sidecar protocol");
  }
  const auto payload_length = read_little_endian<std::uint32_t>(&header[44]);
  if (payload_length > kMaxPayloadBytes) throw std::runtime_error("sidecar payload exceeds limit");
  Message message;
  message.kind = static_cast<MessageKind>(read_little_endian<std::uint16_t>(&header[10]));
  message.request_id = read_little_endian<std::uint64_t>(&header[16]);
  message.width = read_little_endian<std::uint32_t>(&header[24]);
  message.height = read_little_endian<std::uint32_t>(&header[28]);
  message.stride = read_little_endian<std::uint32_t>(&header[32]);
  message.pts_micros = static_cast<std::int64_t>(read_little_endian<std::uint64_t>(&header[36]));
  message.payload.resize(payload_length);
  if (payload_length > 0 && !read_exact(input, message.payload.data(), payload_length)) {
    throw std::runtime_error("truncated sidecar payload");
  }
  return message;
}

void write_message(std::ostream& output, const Message& message) {
  if (message.payload.size() > std::numeric_limits<std::uint32_t>::max()) {
    throw std::runtime_error("sidecar response exceeds protocol limit");
  }
  std::array<std::uint8_t, kHeaderBytes> header{};
  std::copy(kMagic.begin(), kMagic.end(), header.begin());
  write_little_endian<std::uint16_t>(&header[8], kProtocolVersion);
  write_little_endian<std::uint16_t>(&header[10], static_cast<std::uint16_t>(message.kind));
  write_little_endian<std::uint64_t>(&header[16], message.request_id);
  write_little_endian<std::uint32_t>(&header[24], message.width);
  write_little_endian<std::uint32_t>(&header[28], message.height);
  write_little_endian<std::uint32_t>(&header[32], message.stride);
  write_little_endian<std::uint64_t>(&header[36], static_cast<std::uint64_t>(message.pts_micros));
  write_little_endian<std::uint32_t>(&header[44], static_cast<std::uint32_t>(message.payload.size()));
  output.write(reinterpret_cast<const char*>(header.data()), static_cast<std::streamsize>(header.size()));
  output.write(reinterpret_cast<const char*>(message.payload.data()),
               static_cast<std::streamsize>(message.payload.size()));
  output.flush();
  if (!output) throw std::runtime_error("failed to write sidecar response");
}

void write_error(std::ostream& output, std::uint64_t request_id, const std::string& message) {
  write_message(output, Message{
      MessageKind::kErrorResponse,
      request_id,
      0,
      0,
      0,
      0,
      std::vector<std::uint8_t>(message.begin(), message.end()),
  });
}

Options parse_options(int argc, char** argv) {
  Options options;
  bool stdio = false;
  for (int index = 1; index < argc; ++index) {
    const std::string argument = argv[index];
    if (argument == "--stdio") {
      stdio = true;
    } else if (argument == "--model-dir" && index + 1 < argc) {
      options.model_directory = fs::absolute(argv[++index]);
    } else if (argument == "--model-id" && index + 1 < argc) {
      options.model_id = argv[++index];
    } else {
      throw std::runtime_error("invalid sidecar argument: " + argument);
    }
  }
  if (!stdio || options.model_directory.empty() || options.model_id != "rife-v4.6") {
    throw std::runtime_error("--stdio, --model-dir and --model-id rife-v4.6 are required");
  }
  for (const auto* name : {"flownet.param", "flownet.bin"}) {
    const auto path = options.model_directory / name;
    if (!fs::is_regular_file(path) || fs::file_size(path) == 0) {
      throw std::runtime_error("missing RIFE model file: " + path.string());
    }
  }
  return options;
}

std::string escape_json(const std::string& value) {
  std::string escaped;
  escaped.reserve(value.size());
  for (const unsigned char byte : value) {
    switch (byte) {
      case '\\': escaped += "\\\\"; break;
      case '"': escaped += "\\\""; break;
      case '\n': escaped += "\\n"; break;
      case '\r': escaped += "\\r"; break;
      case '\t': escaped += "\\t"; break;
      default:
        if (byte >= 0x20) escaped += static_cast<char>(byte);
    }
  }
  return escaped;
}

std::size_t frame_bytes(const Message& message) {
  if (message.width == 0 || message.height == 0 || message.width > 8192 || message.height > 8192 ||
      message.stride != message.width * 3) {
    throw std::runtime_error("invalid RGB24 frame dimensions");
  }
  const auto bytes = static_cast<std::uint64_t>(message.stride) * message.height;
  if (bytes > kMaxPayloadBytes / 2) throw std::runtime_error("RGB24 frame exceeds limit");
  return static_cast<std::size_t>(bytes);
}

void swap_red_blue(std::uint8_t* pixels, std::size_t length) {
  for (std::size_t offset = 0; offset + 2 < length; offset += 3) {
    std::swap(pixels[offset], pixels[offset + 2]);
  }
}

Message process_frame_pair(const Message& request, RIFE& rife) {
  const auto bytes = frame_bytes(request);
  if (request.payload.size() != bytes * 2) throw std::runtime_error("invalid RGB24 frame pair length");
  std::vector<std::uint8_t> previous(request.payload.begin(), request.payload.begin() + bytes);
  std::vector<std::uint8_t> next(request.payload.begin() + bytes, request.payload.end());
#if defined(_WIN32)
  swap_red_blue(previous.data(), previous.size());
  swap_red_blue(next.data(), next.size());
#endif
  ncnn::Mat previous_frame(static_cast<int>(request.width), static_cast<int>(request.height),
                           previous.data(), static_cast<std::size_t>(3), 1);
  ncnn::Mat next_frame(static_cast<int>(request.width), static_cast<int>(request.height), next.data(),
                       static_cast<std::size_t>(3), 1);
  std::vector<std::uint8_t> output(bytes);
  ncnn::Mat output_frame(static_cast<int>(request.width), static_cast<int>(request.height),
                         output.data(), static_cast<std::size_t>(3), 1);
  const int result = rife.process(previous_frame, next_frame, 0.5F, output_frame);
  if (result != 0) throw std::runtime_error("RIFE inference failed: " + std::to_string(result));
#if defined(_WIN32)
  swap_red_blue(output.data(), output.size());
#endif
  return Message{
      MessageKind::kFrameResponse,
      request.request_id,
      request.width,
      request.height,
      request.stride,
      request.pts_micros,
      std::move(output),
  };
}

}  // namespace

int main(int argc, char** argv) {
  try {
    const Options options = parse_options(argc, argv);
#if defined(_WIN32)
    _setmode(_fileno(stdin), _O_BINARY);
    _setmode(_fileno(stdout), _O_BINARY);
#endif
    ncnn::create_gpu_instance();
    const int gpu_count = ncnn::get_gpu_count();
    if (gpu_count <= 0) throw std::runtime_error("no Vulkan GPU device available");
    const int gpu_index = ncnn::get_default_gpu_index();
    const std::string gpu_name = ncnn::get_gpu_info(gpu_index).device_name();
    RIFE rife(gpu_index, false, false, false, 1, false, true);
#if defined(_WIN32)
    const int loaded = rife.load(options.model_directory.wstring());
#else
    const int loaded = rife.load(options.model_directory.string());
#endif
    if (loaded != 0) throw std::runtime_error("failed to load RIFE model");

    while (true) {
      Message request = read_message(std::cin);
      try {
        if (request.kind == MessageKind::kHandshakeRequest) {
          const std::string payload =
              "{\"ready\":true,\"protocolVersion\":1,\"backend\":\"ncnn-vulkan\","
              "\"gpuDevice\":\"" + escape_json(gpu_name) + "\",\"modelId\":\"rife-v4.6\"}";
          write_message(std::cout, Message{
              MessageKind::kHandshakeResponse,
              request.request_id,
              0,
              0,
              0,
              0,
              std::vector<std::uint8_t>(payload.begin(), payload.end()),
          });
        } else if (request.kind == MessageKind::kWarmupRequest ||
                   request.kind == MessageKind::kInterpolateRequest) {
          write_message(std::cout, process_frame_pair(request, rife));
        } else if (request.kind == MessageKind::kShutdownRequest) {
          break;
        } else {
          write_error(std::cout, request.request_id, "unsupported request kind");
        }
      } catch (const std::exception& error) {
        write_error(std::cout, request.request_id, error.what());
      }
    }
    ncnn::destroy_gpu_instance();
    return 0;
  } catch (const std::exception& error) {
    std::cerr << "[ani-rife-model-sidecar] fatal: " << error.what() << '\n';
    ncnn::destroy_gpu_instance();
    return 1;
  }
}
