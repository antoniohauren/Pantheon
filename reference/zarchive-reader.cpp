// Independent byte and random-access oracle using the unmodified pinned reader.
#include <zarchive/zarchivereader.h>
#include <algorithm>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <vector>

static void require_at(bool ok, int line) {
    if (!ok) throw std::runtime_error("ZArchive comparison failed at line " + std::to_string(line));
}
#define require(ok) require_at((ok), __LINE__)

static void compare(ZArchiveReader& reader, const std::filesystem::path& root,
                    const std::string& path, uint64_t& files, uint64_t& dirs) {
    auto dir = reader.LookUp(path, false, true);
    require(dir != ZARCHIVE_INVALID_NODE);
    uint32_t expected = 0;
    for (const auto& unused : std::filesystem::directory_iterator(root / path)) {
        (void)unused;
        ++expected;
    }
    if (reader.GetDirEntryCount(dir) != expected)
        throw std::runtime_error("directory count: " + path + " actual=" + std::to_string(reader.GetDirEntryCount(dir)) + " expected=" + std::to_string(expected));
    for (uint32_t i = 0; i < expected; ++i) {
        ZArchiveReader::DirEntry entry;
        require(reader.GetDirEntry(dir, i, entry));
        std::string name = path.empty() ? std::string(entry.name) : path + "/" + std::string(entry.name);
        auto host = root / name;
        if (entry.isDirectory) {
            require(std::filesystem::is_directory(host));
            ++dirs;
            compare(reader, root, name, files, dirs);
            continue;
        }
        require(entry.isFile && std::filesystem::is_regular_file(host));
        auto handle = reader.LookUp(name);
        auto size = std::filesystem::file_size(host);
        require(reader.GetFileSize(handle) == size);
        std::string folded = name;
        for (char& c : folded) if (c >= 'a' && c <= 'z') c -= 32;
        require(reader.LookUp(folded) == handle);
        std::ifstream input(host, std::ios::binary);
        std::vector<char> actual(131079), expectedBytes(actual.size());
        auto check = [&](uint64_t offset, uint64_t length) {
            auto count = std::min(length, size - offset);
            require(reader.ReadFromFile(handle, offset, length, actual.data()) == count);
            input.clear();
            input.seekg(offset);
            input.read(expectedBytes.data(), count);
            require(static_cast<uint64_t>(input.gcount()) == count);
            require(std::equal(actual.begin(), actual.begin() + count, expectedBytes.begin()));
        };
        for (uint64_t offset = 0; offset < size; offset += actual.size()) check(offset, actual.size());
        uint64_t seed = 0x12345678;
        for (int j = 0; j < 40; ++j) {
            seed = seed * 6364136223846793005ULL + 1;
            check(seed % (size + 1), 70001);
        }
        check(size, 1);
        ++files;
    }
}

int main(int argc, char** argv) {
    try {
        require(argc == 3);
        std::unique_ptr<ZArchiveReader> reader(ZArchiveReader::OpenFromFile(argv[1]));
        require(bool(reader));
        uint64_t files = 0, dirs = 0;
        compare(*reader, argv[2], "", files, dirs);
        std::cout << files << " files, " << dirs << " directories: bytes and random reads match\n";
    } catch (const std::exception& e) {
        std::cerr << e.what() << '\n';
        return 1;
    }
}
