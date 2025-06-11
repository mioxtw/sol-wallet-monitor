# SOL 錢包監控器

這是一個完整的 SOL 錢包監控 Web 應用程式，包含 Rust 後端 API 和使用 lightweight-charts V5 的前端介面。

## 功能特色

### 後端功能
- 🔄 實時監控 SOL 和 WSOL 錢包餘額
- 📊 歷史餘額數據追蹤
- 🌐 RESTful API 接口
- 🔗 WebSocket 實時推送
- ⚡ gRPC 連接 Solana 網絡獲取即時數據
- 📈 支持多種時間間隔的歷史數據查詢

### 前端功能
- 📱 響應式 Web 界面
- 📊 錢包列表顯示（總餘額到小數點第6位）
- 📈 互動式圖表顯示餘額變化
- 🎛️ 可選擇數據類型：SOL、WSOL 或總餘額（顯示到小數點第4位）
- ⏰ 多種時間範圍：5M、10M、30M、1H、2H、4H、8H、12H、1D、1W、ALL
- 💰 實時顯示 SOL、WSOL 和總餘額
- 🔗 WebSocket 實時連接狀態

## 系統架構

```
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   Solana 網絡    │────│   Rust 後端     │────│   前端界面       │
│                 │gRPC│                │HTTP│                │
│ - SOL 餘額      │    │ - API 端點      │    │ - 錢包列表       │
│ - WSOL 餘額     │    │ - WebSocket     │    │ - 圖表顯示       │
│ - 交易事件      │    │ - 歷史數據      │    │ - 實時更新       │
└─────────────────┘    └─────────────────┘    └─────────────────┘
```

## 安裝與配置

### 1. 先決條件
- Rust 1.70+ 和 Cargo
- 現代 Web 瀏覽器

### 2. 克隆專案
```bash
git clone <repository-url>
cd sol-wallet-monitor
```

### 3. 配置錢包
編輯 `config.toml` 文件，添加您要監控的錢包：

```toml
# SOL 錢包監控配置

# gRPC 服務器配置
[grpc]
endpoint = "http://127.0.0.1:10000"

# Web 服務器配置
[server]
host = "127.0.0.1"
port = 3000

# 監控的錢包列表
[[wallets]]
address = "您的錢包地址1"
name = "錢包名稱1"

[[wallets]]
address = "您的錢包地址2"
name = "錢包名稱2"

# 日誌設定
[logging]
level = "info"
```

### 4. 編譯並運行
```bash
# 編譯程式
cargo build --release

# 運行應用程式
cargo run --release
```

### 5. 訪問 Web 界面
在瀏覽器中打開 `http://127.0.0.1:3000`

## API 文檔

### 錢包列表
```
GET /api/wallets
```
返回所有監控錢包的當前狀態。

**響應示例：**
```json
[
  {
    "address": "7dGrdJRYtsNR8UYxZ3TnifXGjGc9eRYLq9sELwYpuuUu",
    "name": "主要錢包",
    "sol_balance": 171.386164,
    "wsol_balance": 0.0,
    "total_balance": 171.386164,
    "last_update": "2025-06-11T06:31:41Z"
  }
]
```

### 錢包詳細信息
```
GET /api/wallets/{address}
```
返回特定錢包的詳細信息。

### 圖表數據
```
GET /api/chart?wallet={address}&data_type={type}&interval={interval}
```

**參數：**
- `wallet`: 錢包地址
- `data_type`: 數據類型 (`sol`, `wsol`, `total`)
- `interval`: 時間範圍 (`5M`, `10M`, `30M`, `1H`, `2H`, `4H`, `8H`, `12H`, `1D`, `1W`, `ALL`)

**響應示例：**
```json
[
  {
    "time": 1749623496,
    "value": 171.3866
  }
]
```

### WebSocket 連接
```
WS /ws
```
實時推送錢包餘額更新，每秒發送一次最新數據。

## 前端界面說明

### 左側面板 - 錢包列表
- 顯示所有監控錢包
- 總餘額顯示到小數點第6位
- 點擊錢包可查看詳細圖表
- 實時更新餘額變化

### 右側上方 - 餘額概覽
- SOL 餘額
- WSOL 餘額  
- 總餘額（SOL + WSOL）

### 右側下方 - 圖表區域
**控制選項：**
- **數據類型**：選擇顯示 SOL、WSOL 或總餘額
- **時間範圍**：從5分鐘到全部歷史數據

**圖表功能：**
- 實時更新數據
- 互動式縮放和平移
- 懸停顯示詳細數值
- 響應式設計適配移動設備

### 連接狀態指示器
右上角顯示與後端的連接狀態：
- 🟢 已連接：正常接收數據
- 🔴 連接中斷：嘗試重新連接

## 技術詳情

### 後端技術棧
- **Rust**: 主要開發語言
- **Axum**: Web 框架
- **Tokio**: 異步運行時
- **yellowstone-grpc**: Solana gRPC 客戶端
- **Serde**: JSON 序列化
- **Tower-HTTP**: CORS 支持

### 前端技術棧
- **HTML5/CSS3**: 基礎結構和樣式
- **JavaScript ES6+**: 交互邏輯
- **lightweight-charts V5**: 圖表庫
- **WebSocket**: 實時通信

### 數據流
1. **數據獲取**: 通過 gRPC 從 Solana 網絡獲取實時交易數據
2. **數據處理**: Rust 後端解析交易，更新錢包餘額
3. **數據存儲**: 內存中保存最近 10,000 條歷史記錄
4. **API 服務**: RESTful API 提供錢包數據和歷史圖表數據
5. **實時推送**: WebSocket 推送最新餘額到前端
6. **圖表渲染**: 前端使用 lightweight-charts 渲染互動圖表

## 性能優化

- 🚀 內存中歷史數據緩存
- ⚡ 非阻塞異步處理
- 📊 按需圖表數據過濾
- 🔄 自動重連機制
- 💾 限制歷史數據大小避免內存溢出

## 故障排除

### 常見問題

**1. 無法連接到 gRPC 端點**
- 檢查 `config.toml` 中的 `grpc.endpoint` 設置
- 確保 gRPC 服務器正在運行

**2. 錢包餘額未更新**
- 檢查錢包地址是否正確
- 查看日誌了解詳細錯誤信息

**3. Web 界面無法訪問**
- 確認端口 3000 未被占用
- 檢查防火牆設置

**4. 圖表數據為空**
- 等待一段時間讓系統收集歷史數據
- 檢查錢包是否有交易活動

### 調試模式
設置環境變量開啟詳細日誌：
```bash
RUST_LOG=debug cargo run
```

## 擴展功能建議

- 📧 餘額變化警報通知
- 💾 持久化數據存儲
- 📱 移動應用程式
- 🔐 用戶認證與授權
- 📊 更多圖表類型和指標
- 🎯 自定義監控閾值
- 📈 技術分析指標

## 貢獻

歡迎提交 issue 和 pull request 來改進這個專案！

## 許可證

[在此添加您的許可證信息]

---

**注意**: 此應用程式僅用於監控目的，不提供財務建議。請謹慎使用並保護好您的錢包私鑰。 