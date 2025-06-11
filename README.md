# SOL 錢包監控程式 (精簡版)

這是一個精簡的 Solana 錢包餘額監控程式，透過 Yellowstone gRPC 即時監聽多個錢包的 SOL 和 WSOL 餘額變化。

## 功能特色

- 🚀 **即時監聽**: 透過 Yellowstone gRPC 即時監聽區塊鏈交易
- 💰 **多錢包支援**: 可同時監控多個 SOL 錢包
- 📊 **餘額追蹤**: 追蹤 SOL、WSOL 和總餘額
- ⚙️ **配置靈活**: 透過 config.toml 輕鬆設定監控錢包
- 🔄 **自動重連**: 網路中斷時自動重新連接

## 快速開始

### 1. 安裝依賴
```bash
cargo build
```

### 2. 設定配置檔案
編輯 `config.toml` 檔案，設定要監控的錢包：

```toml
# gRPC 服務器配置
[grpc]
endpoint = "http://127.0.0.1:10000"

# 監控的錢包列表
[[wallets]]
address = "7dGrdJRYtsNR8UYxZ3TnifXGjGc9eRYLq9sELwYpuuUu"
name = "主要錢包"

[[wallets]]
address = "As516ZAsiAzTQuR5JTP5oEucPb3irbQf4tBxKx3MDMpa"
name = "測試錢包"

# 日誌設定
[logging]
level = "info"
```

### 3. 執行程式
```bash
cargo run
```

## 輸出範例

程式會顯示如下格式的餘額資訊：

```
[2024-01-01T12:00:00Z INFO] 🚀 SOL 錢包監控程式啟動
[2024-01-01T12:00:00Z INFO] 📍 gRPC 端點: http://127.0.0.1:10000
[2024-01-01T12:00:00Z INFO] 📝 已加入監控錢包: 主要錢包 (7dGrdJRY)
[2024-01-01T12:00:00Z INFO] ✅ 成功連接到 gRPC 服務器
[2024-01-01T12:00:00Z INFO] 💰 初始化 | 主要錢包 (7dGrdJRY) | SOL: 1.234567 | WSOL: 5.678901 | 總計: 6.913468
[2024-01-01T12:00:00Z INFO] 🔄 收到交易: 2Z8...abc
[2024-01-01T12:00:00Z INFO] 💰 主要錢包 SOL 變化: -0.000005000
[2024-01-01T12:00:00Z INFO] 💰 餘額更新 | 主要錢包 (7dGrdJRY) | SOL: 1.234562 | WSOL: 5.678901 | 總計: 6.913463
```

## 配置選項

### gRPC 設定
- `endpoint`: Yellowstone gRPC 服務器端點

### 錢包設定
- `address`: 錢包的公鑰地址
- `name`: 錢包的顯示名稱

### 日誌設定
- `level`: 日誌等級 (trace, debug, info, warn, error)

## 系統需求

- Rust 1.85.0+
- 可用的 Yellowstone gRPC 服務器
- 網路連接到 Solana 主網

## 故障排除

### 連接問題
1. 確認 gRPC 服務器正確運行在指定端點
2. 檢查防火牆設定
3. 驗證網路連接

### 配置問題
1. 確認 `config.toml` 格式正確
2. 驗證錢包地址有效
3. 檢查 gRPC 端點設定

## 技術實現

- **語言**: Rust
- **gRPC 客戶端**: yellowstone-grpc-client
- **異步運行時**: Tokio
- **配置解析**: TOML
- **日誌**: env_logger

## 與完整版的差異

這是原完整系統的精簡版本，移除了：
- Web 服務器和前端介面
- 數據庫持久化
- WebSocket 功能
- REST API

只保留核心的 gRPC 監聽和餘額輸出功能，適合命令行使用和系統整合。 