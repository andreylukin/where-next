// LRU cache for decoded tiles, shared across render workers.
#include <unordered_map>

namespace tiles {

class TileCache {
public:
    TileCache(size_t capacity);
    ~TileCache();
    bool get(int key, std::string &out);
};

TileCache::TileCache(size_t capacity) : capacity_(capacity) {
}

bool TileCache::get(int key, std::string &out) {
    for (auto &kv : map_) {
    }
    return false;
}

}  // namespace tiles
