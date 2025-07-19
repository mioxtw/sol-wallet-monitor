use {
    axum::{
        extract::{Path, Query, ws::{WebSocket, WebSocketUpgrade}},
        http::StatusCode,
        response::{Html, Response},
        routing::{get, post, delete},
        Json, Router,
    },
    bs58,
    chrono::{DateTime, Utc},
    futures::{stream::StreamExt, sink::SinkExt},
    log::{debug, error, info, warn},
    redb::{Database, TableDefinition, ReadableTable},
    serde::{Deserialize, Serialize},
    solana_client::rpc_client::RpcClient,
    solana_program::{program_pack::Pack, pubkey::Pubkey as ProgramPubkey},
    solana_sdk::pubkey::Pubkey,
    spl_associated_token_account::get_associated_token_address,
    spl_token::state::Account as TokenAccount,
    std::{
        collections::{HashMap, VecDeque},
        fs,
        str::FromStr,
        sync::{Arc, Mutex},
        time::Duration,
    },

    tower_http::cors::CorsLayer,
    yellowstone_grpc_client::GeyserGrpcClient,
    yellowstone_grpc_proto::{
        geyser::SubscribeUpdate,
        prelude::{
            CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
            subscribe_update::UpdateOneof,
        },
    },
};

// 常數定義
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const MAX_HISTORY_SIZE: usize = 10000000;
const DB_FILE: &str = "wallet_history.redb";

// 資料庫表格定義
const WALLET_HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("wallet_history");

// API 相關結構
#[derive(Debug, Serialize, Deserialize)]
struct WalletSummary {
    address: String,
    name: String,
    sol_balance: f64,
    wsol_balance: f64,
    total_balance: f64,
    last_update: DateTime<Utc>,
    sampled_history: Vec<BalanceHistory>, // 採樣後的歷史數據
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BalanceHistory {
    timestamp: DateTime<Utc>,
    sol_balance: f64,
    wsol_balance: f64,
    total_balance: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChartDataPoint {
    time: i64, // Unix timestamp in seconds
    value: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WalletHistoryRecord {
    timestamp: DateTime<Utc>,
    address: String,
    sol_balance: f64,
    wsol_balance: f64,
    total_balance: f64,
}

impl WalletHistoryRecord {
    fn new(address: String, sol_balance: f64, wsol_balance: f64) -> Self {
        Self {
            timestamp: Utc::now(),
            address,
            sol_balance,
            wsol_balance,
            total_balance: sol_balance + wsol_balance,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChartQueryParams {
    wallet: String,
    data_type: String, // "sol", "wsol", or "total"
    interval: String,  // "5M", "10M", "30M", "1H", "2H", "4H", "8H", "12H", "1D", "1W", "ALL"
}

#[derive(Debug, Deserialize)]
struct AddWalletRequest {
    name: String,
    address: String,
}

#[derive(Debug, Serialize)]
struct ApiResponse {
    success: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

// 配置結構
#[derive(Debug, Deserialize, Clone)]
struct Config {
    grpc: GrpcConfig,
    rpc: RpcConfig,
    wallets: Vec<WalletConfig>,
    logging: LoggingConfig,
    server: ServerConfig,
}

#[derive(Debug, Deserialize, Clone)]
struct GrpcConfig {
    endpoint: String,
}

#[derive(Debug, Deserialize, Clone)]
struct RpcConfig {
    endpoint: String,
}

#[derive(Debug, Deserialize, Clone)]
struct WalletConfig {
    address: String,
    name: String,
}

#[derive(Debug, Deserialize, Clone)]
struct LoggingConfig {
    level: String,
}

#[derive(Debug, Deserialize, Clone)]
struct ServerConfig {
    host: String,
    port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3000,
        }
    }
}

// 錢包餘額追蹤器
#[derive(Debug, Clone)]
struct WalletBalance {
    address: String,
    name: String,
    sol_balance: f64,
    wsol_balance: f64,
    wsol_initialized: bool,
    last_update: DateTime<Utc>,
    history: VecDeque<BalanceHistory>,
}

impl WalletBalance {
    fn new(address: String, name: String) -> Self {
        Self {
            address,
            name,
            sol_balance: 0.0,
            wsol_balance: 0.0,
            wsol_initialized: false,
            last_update: Utc::now(),
            history: VecDeque::new(),
        }
    }

    fn update_sol(&mut self, lamports: u64) {
        self.sol_balance = lamports as f64 / 1_000_000_000.0;
        self.last_update = Utc::now();
        // 只有在WSOL已初始化後才記錄歷史
        if self.wsol_initialized {
            self.add_to_history();
        }
    }

    fn update_wsol(&mut self, amount: f64) {
        self.wsol_balance = amount;
        self.wsol_initialized = true;
        self.last_update = Utc::now();
        // 只有在WSOL已初始化後才記錄歷史
        if self.wsol_initialized {
            self.add_to_history();
        }
    }

    fn initialize_wsol(&mut self, amount: f64) {
        self.wsol_balance = amount;
        self.wsol_initialized = true;
        self.last_update = Utc::now();
        // 只有在沒有歷史記錄時才添加第一條記錄
        if self.history.is_empty() {
            self.add_to_history();
        }
    }

    fn total_balance(&self) -> f64 {
        if !self.wsol_initialized {
            self.sol_balance
        } else {
            self.sol_balance + self.wsol_balance
        }
    }

    fn add_to_history(&mut self) {
        let history_point = BalanceHistory {
            timestamp: self.last_update,
            sol_balance: self.sol_balance,
            wsol_balance: if self.wsol_initialized { self.wsol_balance } else { 0.0 },
            total_balance: self.total_balance(),
        };

        self.history.push_back(history_point);
        
        // 限制歷史記錄大小
        while self.history.len() > MAX_HISTORY_SIZE {
            self.history.pop_front();
        }
    }

    fn load_history_from_db(&mut self, records: Vec<WalletHistoryRecord>) {
        self.history.clear();
        for record in records {
            let history_point = BalanceHistory {
                timestamp: record.timestamp,
                sol_balance: record.sol_balance,
                wsol_balance: record.wsol_balance,
                total_balance: record.total_balance,
            };
            self.history.push_back(history_point);
        }
        
        // 注意：不從歷史記錄設置餘額，因為WSOL餘額可能過時
        // 餘額將從RPC重新獲取以確保準確性
        
        // 限制歷史記錄大小
        while self.history.len() > MAX_HISTORY_SIZE {
            self.history.pop_front();
        }
    }

    fn to_summary(&self) -> WalletSummary {
        // 對歷史數據進行採樣到100筆
        let sampled_history = if self.history.len() > 100 {
            let step = self.history.len() / 100;
            self.history.iter()
                .enumerate()
                .filter(|(i, _)| i % step == 0)
                .map(|(_, h)| h.clone())
                .take(100)
                .collect()
        } else {
            self.history.iter().cloned().collect()
        };
        
        WalletSummary {
            address: self.address.clone(),
            name: self.name.clone(),
            sol_balance: self.sol_balance,
            wsol_balance: if self.wsol_initialized { self.wsol_balance } else { 0.0 },
            total_balance: self.total_balance(),
            last_update: self.last_update,
            sampled_history,
        }
    }

    fn print_balance(&self, reason: &str) {
        if !self.wsol_initialized {
            info!(
                "💰 {} | {} ({}) | SOL: {:.6} | WSOL: 未初始化 | 總計: {:.6}",
                reason,
                self.name,
                &self.address[..8],
                self.sol_balance,
                self.total_balance()
            );
        } else {
            info!(
                "💰 {} | {} ({}) | SOL: {:.6} | WSOL: {:.6} | 總計: {:.6}",
                reason,
                self.name,
                &self.address[..8],
                self.sol_balance,
                self.wsol_balance,
                self.total_balance()
            );
        }
    }
}

type SharedWallets = Arc<Mutex<HashMap<String, WalletBalance>>>;
type SharedDatabase = Arc<Database>;

// gRPC 流重啟信號
type GrpcRestartSignal = Arc<Mutex<bool>>;

// 應用狀態結構
#[derive(Clone)]
struct AppState {
    wallets: SharedWallets,
    database: SharedDatabase,
    grpc_restart_signal: GrpcRestartSignal,
    config: Config,
}

// 資料庫操作函數
fn initialize_database() -> Result<Database, Box<dyn std::error::Error>> {
    let db = Database::create(DB_FILE)?;
    info!("📊 資料庫已初始化: {}", DB_FILE);
    Ok(db)
}

fn save_wallet_history(db: &Database, record: &WalletHistoryRecord) -> Result<(), Box<dyn std::error::Error>> {
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(WALLET_HISTORY_TABLE)?;
        let key = format!("{}_{}", record.address, record.timestamp.timestamp_millis());
        let value = serde_json::to_string(record)?;
        table.insert(key.as_str(), value.as_str())?;
    }
    write_txn.commit()?;
    Ok(())
}

fn load_wallet_history(db: &Database, address: &str) -> Result<Vec<WalletHistoryRecord>, Box<dyn std::error::Error>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(WALLET_HISTORY_TABLE)?;
    let mut records = Vec::new();
    
    let prefix = format!("{}_", address);
    let mut iter = table.iter()?;
    
    while let Some(entry) = iter.next() {
        let (key, value) = entry?;
        let key_str = key.value();
        if key_str.starts_with(&prefix) {
            let record: WalletHistoryRecord = serde_json::from_str(value.value())?;
            records.push(record);
        }
    }
    
    // 按時間排序
    records.sort_by_key(|r| r.timestamp);
    Ok(records)
}

fn delete_wallet_history(db: &Database, address: &str) -> Result<(), Box<dyn std::error::Error>> {
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(WALLET_HISTORY_TABLE)?;
        let prefix = format!("{}_", address);
        
        // 收集需要刪除的鍵
        let mut keys_to_delete = Vec::new();
        let mut iter = table.iter()?;
        while let Some(entry) = iter.next() {
            let (key, _) = entry?;
            let key_str = key.value();
            if key_str.starts_with(&prefix) {
                keys_to_delete.push(key_str.to_string());
            }
        }
        
        // 刪除找到的鍵
        for key in keys_to_delete {
            table.remove(key.as_str())?;
        }
    }
    write_txn.commit()?;
    info!("🗑️ 已刪除錢包 {} 的歷史數據", address);
    Ok(())
}

fn load_all_wallet_history(db: &Database) -> Result<HashMap<String, Vec<WalletHistoryRecord>>, Box<dyn std::error::Error>> {
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(WALLET_HISTORY_TABLE)?;
    let mut wallet_records: HashMap<String, Vec<WalletHistoryRecord>> = HashMap::new();
    
    let mut iter = table.iter()?;
    while let Some(entry) = iter.next() {
        let (_, value) = entry?;
        let record: WalletHistoryRecord = serde_json::from_str(value.value())?;
        wallet_records.entry(record.address.clone()).or_insert_with(Vec::new).push(record);
    }
    
    // 對每個錢包的記錄按時間排序
    for records in wallet_records.values_mut() {
        records.sort_by_key(|r| r.timestamp);
    }
    
    Ok(wallet_records)
}

// Web API handlers
async fn get_wallets(axum::extract::State(state): axum::extract::State<AppState>) -> Json<Vec<WalletSummary>> {
    let wallets_guard = state.wallets.lock().unwrap();
    let summaries: Vec<WalletSummary> = wallets_guard.values().map(|w| w.to_summary()).collect();
    Json(summaries)
}

async fn get_wallet_detail(
    Path(address): Path<String>,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<Json<WalletSummary>, StatusCode> {
    let wallets_guard = state.wallets.lock().unwrap();
    match wallets_guard.get(&address) {
        Some(wallet) => Ok(Json(wallet.to_summary())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn get_chart_data(
    Query(params): Query<ChartQueryParams>,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<Json<Vec<ChartDataPoint>>, StatusCode> {
    let wallets_guard = state.wallets.lock().unwrap();
    let wallet = wallets_guard.get(&params.wallet).ok_or(StatusCode::NOT_FOUND)?;
    
    // 獲取所有歷史數據
    let mut history: Vec<_> = wallet.history.iter().collect();
    
    // 排序歷史數據以確保時間順序
    history.sort_by_key(|h| h.timestamp);
    
    // 根據時間範圍過濾數據
    let now = Utc::now();
    let filtered_history: Vec<_> = match params.interval.as_str() {
        "5M" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_minutes() <= 5).collect(),
        "10M" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_minutes() <= 10).collect(),
        "30M" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_minutes() <= 30).collect(),
        "1H" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_hours() <= 1).collect(),
        "2H" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_hours() <= 2).collect(),
        "4H" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_hours() <= 4).collect(),
        "8H" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_hours() <= 8).collect(),
        "12H" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_hours() <= 12).collect(),
        "1D" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_days() <= 1).collect(),
        "1W" => history.into_iter().filter(|h| now.signed_duration_since(h.timestamp).num_weeks() <= 1).collect(),
        "ALL" | _ => history,
    };
    
    let mut chart_data: Vec<ChartDataPoint> = filtered_history
        .iter()
        .filter_map(|h| {
            let value = match params.data_type.as_str() {
                "sol" => h.sol_balance,
                "wsol" => h.wsol_balance,
                "total" => h.total_balance,
                _ => h.total_balance,
            };
            
            // 過濾掉無效數值
            if value.is_finite() && !value.is_nan() {
                Some(ChartDataPoint {
                    time: h.timestamp.timestamp(),
                    value,
                })
            } else {
                None
            }
        })
        .collect();
    
    // 去除重複時間戳（保留最新的）
    chart_data.dedup_by_key(|point| point.time);
    
        // 基於時間的均勻採樣到 1000 筆數據
    let sampled_data = if chart_data.len() > 1000 {
        if chart_data.is_empty() {
            chart_data
        } else {
            let start_time = chart_data.first().unwrap().time;
            let end_time = chart_data.last().unwrap().time;
            let time_span = end_time - start_time;
            
            if time_span <= 0 {
                // 如果時間跨度為0，直接返回原數據
                chart_data
            } else {
                let mut sampled = Vec::new();
                let sample_interval = time_span as f64 / 999.0; // 999個間隔產生1000個點
                
                for i in 0..1000 {
                    let target_time = start_time + (i as f64 * sample_interval) as i64;
                    
                    // 找到最接近目標時間的數據點
                    let closest_point = chart_data.iter()
                        .min_by_key(|point| (point.time - target_time).abs())
                        .unwrap();
                    
                    sampled.push(closest_point.clone());
                }
                
                // 去除重複的時間點，保持時間順序
                sampled.sort_by_key(|point| point.time);
                sampled.dedup_by_key(|point| point.time);
                
                info!("📊 圖表數據時間採樣: 原始 {} 點 -> 採樣 {} 點 (時間跨度: {}秒)", 
                      chart_data.len(), sampled.len(), time_span);
                sampled
            }
        }
    } else {
        info!("📊 圖表數據無需採樣: {} 點 (上限: 1000 點)", chart_data.len());
        chart_data
    };

    info!("📊 圖表數據準備完成: {} 點 (時間範圍: {})", sampled_data.len(), params.interval);
    
    Ok(Json(sampled_data))
}

async fn add_wallet(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(request): Json<AddWalletRequest>,
) -> Result<Json<ApiResponse>, (StatusCode, Json<ErrorResponse>)> {
    let name = request.name.trim();
    let address = request.address.trim();
    
    // 驗證輸入
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse {
            error: "錢包名稱不能為空".to_string(),
        })));
    }
    
    if address.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse {
            error: "錢包地址不能為空".to_string(),
        })));
    }
    
    if address.len() < 32 || address.len() > 44 {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse {
            error: "錢包地址長度不正確".to_string(),
        })));
    }
    
    // 檢查錢包是否已存在
    {
        let wallets_guard = state.wallets.lock().unwrap();
        if wallets_guard.contains_key(address) {
            return Err((StatusCode::CONFLICT, Json(ErrorResponse {
                error: "此錢包地址已存在".to_string(),
            })));
        }
        
        // 檢查名稱是否已存在
        for wallet in wallets_guard.values() {
            if wallet.name == name {
                return Err((StatusCode::CONFLICT, Json(ErrorResponse {
                    error: "此錢包名稱已存在".to_string(),
                })));
            }
        }
    }
    
    // 創建新錢包
    let new_wallet = WalletBalance::new(address.to_string(), name.to_string());
    
    // 嘗試初始化錢包餘額 (使用配置中的RPC端點)
    let rpc_endpoint = &state.config.rpc.endpoint;
    match query_wallet_balance(address, rpc_endpoint).await {
        Ok((sol_balance, wsol_balance)) => {
            let mut new_wallet = new_wallet;
            new_wallet.update_sol((sol_balance * 1_000_000_000.0) as u64);
            new_wallet.initialize_wsol(wsol_balance);
            
            // 添加到錢包列表
            // 保存初始記錄到資料庫
            let initial_record = WalletHistoryRecord::new(
                address.to_string(),
                new_wallet.sol_balance,
                new_wallet.wsol_balance,
            );
            if let Err(e) = save_wallet_history(&state.database, &initial_record) {
                warn!("⚠️ 保存初始歷史記錄失敗: {}", e);
            }

            {
                let mut wallets_guard = state.wallets.lock().unwrap();
                wallets_guard.insert(address.to_string(), new_wallet);
            }
            
            // 更新配置文件
            if let Err(e) = update_config_file(address, name).await {
                warn!("⚠️ 更新配置文件失敗: {}", e);
            }
            
            // 觸發 gRPC 流重啟以訂閱新錢包
            {
                let mut restart_signal = state.grpc_restart_signal.lock().unwrap();
                *restart_signal = true;
            }
            
            info!("✅ 成功新增錢包: {} ({}) - 正在重啟gRPC訂閱", name, &address[..8]);
            
            Ok(Json(ApiResponse {
                success: true,
                message: format!("成功新增錢包 {}", name),
            }))
        }
        Err(e) => {
            error!("❌ 初始化錢包餘額失敗: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
                error: "無法獲取錢包餘額，請檢查地址是否正確".to_string(),
            })))
        }
    }
}

async fn delete_wallet(
    Path(address): Path<String>,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<Json<ApiResponse>, (StatusCode, Json<ErrorResponse>)> {
    let wallet_name = {
        let mut wallets_guard = state.wallets.lock().unwrap();
        if let Some(wallet) = wallets_guard.remove(&address) {
            wallet.name.clone()
        } else {
            return Err((StatusCode::NOT_FOUND, Json(ErrorResponse {
                error: "錢包不存在".to_string(),
            })));
        }
    };
    
    // 刪除資料庫中的歷史記錄
    if let Err(e) = delete_wallet_history(&state.database, &address) {
        warn!("⚠️ 刪除錢包歷史記錄失敗: {}", e);
    }
    
    // 更新配置文件
    if let Err(e) = remove_from_config_file(&address).await {
        warn!("⚠️ 更新配置文件失敗: {}", e);
    }
    
    // 觸發 gRPC 流重啟以停止訂閱已刪除的錢包
    {
        let mut restart_signal = state.grpc_restart_signal.lock().unwrap();
        *restart_signal = true;
    }
    
    info!("✅ 成功刪除錢包: {} ({}) - 正在重啟gRPC訂閱", wallet_name, &address[..8]);
    
    Ok(Json(ApiResponse {
        success: true,
        message: format!("成功刪除錢包 {}", wallet_name),
    }))
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    ws.on_upgrade(|socket| websocket_connection(socket, state.wallets))
}

async fn websocket_connection(mut socket: WebSocket, wallets: SharedWallets) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut last_sent_data: Option<HashMap<String, (f64, f64, f64, DateTime<Utc>)>> = None; // address -> (sol, wsol, total, timestamp)
    
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let current_data: HashMap<String, (f64, f64, f64, DateTime<Utc>)> = {
                    let wallets_guard = wallets.lock().unwrap();
                    wallets_guard.iter().map(|(addr, wallet)| {
                        (addr.clone(), (wallet.sol_balance, wallet.wsol_balance, wallet.total_balance(), wallet.last_update))
                    }).collect()
                };
                
                // 檢查變化並收集更新的錢包
                let mut updates = Vec::new();
                
                for (address, (sol, wsol, total, timestamp)) in &current_data {
                    let has_change = match &last_sent_data {
                        None => true, // 第一次發送
                        Some(last_data) => {
                            match last_data.get(address) {
                                None => true, // 新錢包
                                Some((last_sol, last_wsol, last_total, last_timestamp)) => {
                                    // 檢查餘額或時間戳是否有變化
                                    (sol - last_sol).abs() > f64::EPSILON ||
                                    (wsol - last_wsol).abs() > f64::EPSILON ||
                                    (total - last_total).abs() > f64::EPSILON ||
                                    timestamp != last_timestamp
                                }
                            }
                        }
                    };
                    
                    if has_change {
                        // 獲取錢包詳細信息
                        let wallets_guard = wallets.lock().unwrap();
                        if let Some(wallet) = wallets_guard.get(address) {
                            // 只發送最新的一筆歷史數據
                            let latest_history = wallet.history.back().cloned();
                            
                            let update = serde_json::json!({
                                "type": "update",
                                "wallet": {
                                    "address": wallet.address,
                                    "name": wallet.name,
                                    "sol_balance": wallet.sol_balance,
                                    "wsol_balance": if wallet.wsol_initialized { wallet.wsol_balance } else { 0.0 },
                                    "total_balance": wallet.total_balance(),
                                    "last_update": wallet.last_update,
                                    "latest_data": latest_history.map(|h| serde_json::json!({
                                        "time": h.timestamp.timestamp(),
                                        "sol_balance": h.sol_balance,
                                        "wsol_balance": h.wsol_balance,
                                        "total_balance": h.total_balance
                                    }))
                                }
                            });
                            updates.push(update);
                        }
                    }
                }
                
                // 檢查是否有錢包被刪除
                if let Some(ref last_data) = last_sent_data {
                    for address in last_data.keys() {
                        if !current_data.contains_key(address) {
                            let delete_update = serde_json::json!({
                                "type": "delete",
                                "address": address
                            });
                            updates.push(delete_update);
                        }
                    }
                }
                
                // 發送更新
                if !updates.is_empty() {
                    let message = serde_json::json!({
                        "type": "batch_update",
                        "updates": updates
                    });
                    
                    if socket.send(axum::extract::ws::Message::Text(message.to_string())).await.is_err() {
                        break;
                    }
                    
                    info!("📡 WebSocket 發送 {} 個錢包更新 (只含最新數據)", updates.len());
                    last_sent_data = Some(current_data);
                } else {
                    debug!("📡 WebSocket 無變化，跳過發送");
                }
            }
            msg = socket.recv() => {
                if msg.is_none() {
                    break;
                }
            }
        }
    }
}

async fn serve_index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

// 讀取配置檔案
fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string("config.toml")?;
    let mut config: Config = toml::from_str(&config_content)?;
    
    // 如果沒有server配置，使用默認值
    if config_content.find("[server]").is_none() {
        config.server = ServerConfig::default();
    }
    
    Ok(config)
}

// 設定日誌
fn setup_logging(level: &str) {
    std::env::set_var("RUST_LOG", level);
    env_logger::init();
}

// 計算錢包的 WSOL ATA 地址
fn calculate_wsol_ata(wallet_address: &str) -> Result<String, Box<dyn std::error::Error>> {
    let wallet = ProgramPubkey::from_str(wallet_address)?;
    let wsol_mint = ProgramPubkey::from_str(WSOL_MINT)?;
    let associated_program_id = ProgramPubkey::from_str(ASSOCIATED_TOKEN_PROGRAM_ID)?;
    let token_program_id = ProgramPubkey::from_str(TOKEN_PROGRAM_ID)?;
    
    let (ata, _) = ProgramPubkey::find_program_address(
        &[wallet.as_ref(), token_program_id.as_ref(), wsol_mint.as_ref()],
        &associated_program_id,
    );
    
    Ok(ata.to_string())
}

// 計算所有錢包的 WSOL ATA 地址
fn calculate_all_wsol_atas(wallet_addresses: &[String]) -> Vec<String> {
    let mut ata_addresses = Vec::new();
    
    for wallet_address in wallet_addresses {
        match calculate_wsol_ata(wallet_address) {
            Ok(ata) => {
                info!("💎 錢包 {} 的 WSOL ATA: {}", &wallet_address[..8], &ata[..8]);
                ata_addresses.push(ata);
            }
            Err(e) => {
                error!("❌ 計算錢包 {} 的 WSOL ATA 失敗: {}", wallet_address, e);
            }
        }
    }
    
    ata_addresses
}

// 處理 WSOL Account 更新
fn handle_wsol_account_update(
    update: SubscribeUpdate,
    wallets: &mut HashMap<String, WalletBalance>,
    ata_to_wallet_map: &HashMap<String, String>,
    db: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(UpdateOneof::Account(account_update)) = update.update_oneof {
        if let Some(account) = account_update.account {
            let ata_address = bs58::encode(&account.pubkey).into_string();
            
            // 檢查是否是我們監聽的 ATA 地址
            if let Some(wallet_address) = ata_to_wallet_map.get(&ata_address) {
                // 解析 token account 數據
                match TokenAccount::unpack(&account.data) {
                    Ok(token_account) => {
                        let wsol_balance = token_account.amount as f64 / 1_000_000_000.0; // WSOL decimals = 9
                        
                        if let Some(wallet) = wallets.get_mut(wallet_address) {
                            let old_balance = wallet.wsol_balance;
                            wallet.update_wsol(wsol_balance);
                            
                            if (wsol_balance - old_balance).abs() > 0.000001 {
                                info!("💎 錢包 {} WSOL 餘額變化: {:.9} SOL (從 {:.9} 到 {:.9})", 
                                      &wallet_address[..8], 
                                      wsol_balance - old_balance, 
                                      old_balance, 
                                      wsol_balance);
                                
                                wallet.print_balance("WSOL帳戶更新");
                                
                                // 保存到資料庫
                                let record = WalletHistoryRecord::new(
                                    wallet.address.clone(),
                                    wallet.sol_balance,
                                    wallet.wsol_balance,
                                );
                                if let Err(e) = save_wallet_history(db, &record) {
                                    warn!("⚠️ 保存WSOL帳戶更新記錄失敗 {}: {}", wallet.name, e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ 解析 token account 數據失敗: {}", e);
                    }
                }
            }
        }
    }
    Ok(())
}

// 查詢錢包餘額 (初始化用)
async fn query_wallet_balance(wallet_address: &str, rpc_endpoint: &str) -> Result<(f64, f64), Box<dyn std::error::Error + Send + Sync>> {
    // 使用 Solana RPC Client
    let client = RpcClient::new(rpc_endpoint.to_string());
    
    // 解析錢包地址
    let owner_pubkey = Pubkey::from_str(wallet_address)?;
    
    // 查詢 SOL 餘額
    let sol_lamports = client.get_balance(&owner_pubkey)?;
    let sol_balance = sol_lamports as f64 / 1_000_000_000.0;
    
    // 查詢 WSOL 餘額 - 使用 ATA 方式
    let wsol_mint = Pubkey::from_str(WSOL_MINT)?;
    let ata = get_associated_token_address(&owner_pubkey, &wsol_mint);
    
    let wsol_balance = match client.get_token_account_balance(&ata) {
        Ok(balance) => balance.ui_amount.unwrap_or(0.0),
        Err(_) => 0.0, // ATA 不存在，餘額為 0
    };
    
    Ok((sol_balance, wsol_balance))
}

// 從RPC初始化所有錢包餘額
async fn initialize_wallets_from_rpc(wallets: &mut HashMap<String, WalletBalance>, db: &Database, rpc_endpoint: &str) {
    let wallet_count = wallets.len();
    info!("🔄 開始從RPC獲取 {} 個錢包的最新餘額 (使用ATA查詢)", wallet_count);
    
    let mut processed_count = 0;
    
    for (address, wallet) in wallets.iter_mut() {
        processed_count += 1;
        info!("📋 正在獲取錢包 {}/{} 的最新餘額: {} ({})", processed_count, wallet_count, wallet.name, &address[..8]);
        
        // 從RPC獲取最新的SOL和WSOL餘額
        match query_wallet_balance(address, rpc_endpoint).await {
            Ok((sol_balance, wsol_balance)) => {
                wallet.update_sol((sol_balance * 1_000_000_000.0) as u64);
                wallet.initialize_wsol(wsol_balance);
                info!("   📊 最新餘額 - SOL: {:.6}, WSOL: {:.6}", sol_balance, wsol_balance);
            }
            Err(e) => {
                error!("❌ 獲取錢包 {} 的SOL和WSOL餘額失敗: {}", wallet.name, e);
                // 設置為0以避免未初始化狀態
                wallet.initialize_wsol(0.0);
            }
        }
        
        wallet.print_balance("RPC初始化");
        
        // 保存最新餘額記錄到資料庫
        if wallet.wsol_initialized {
            let current_record = WalletHistoryRecord::new(
                wallet.address.clone(),
                wallet.sol_balance,
                wallet.wsol_balance,
            );
            if let Err(e) = save_wallet_history(db, &current_record) {
                warn!("⚠️ 保存最新餘額記錄失敗 {}: {}", wallet.name, e);
            }
        }
    }
    
    info!("✅ 所有錢包的最新餘額獲取完成！(無需等待間隔)");
}



// 處理 SOL Account 更新
fn handle_sol_account_update(
    update: SubscribeUpdate,
    wallets: &mut HashMap<String, WalletBalance>,
    wallet_addresses: &[String],
    db: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(UpdateOneof::Account(account_update)) = update.update_oneof {
        if let Some(account) = account_update.account {
            let wallet_address = bs58::encode(&account.pubkey).into_string();
            
            // 檢查是否是我們監聽的錢包地址
            if wallet_addresses.contains(&wallet_address) {
                if let Some(wallet) = wallets.get_mut(&wallet_address) {
                    let old_balance = wallet.sol_balance;
                    wallet.update_sol(account.lamports);
                    
                    if (wallet.sol_balance - old_balance).abs() > 0.000001 {
                        info!("💰 錢包 {} SOL 餘額變化: {:.6} SOL (從 {:.6} 到 {:.6})", 
                              &wallet_address[..8], 
                              wallet.sol_balance - old_balance, 
                              old_balance, 
                              wallet.sol_balance);
                        
                        wallet.print_balance("SOL帳戶更新");
                        
                        // 保存到資料庫
                        let record = WalletHistoryRecord::new(
                            wallet.address.clone(),
                            wallet.sol_balance,
                            wallet.wsol_balance,
                        );
                        if let Err(e) = save_wallet_history(db, &record) {
                            warn!("⚠️ 保存SOL帳戶更新記錄失敗 {}: {}", wallet.name, e);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// 配置文件操作函數
async fn update_config_file(address: &str, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string("config.toml")?;
    let mut lines: Vec<String> = config_content.lines().map(|s| s.to_string()).collect();
    
    // 添加新的錢包配置
    lines.push(String::new());
    lines.push("[[wallets]]".to_string());
    lines.push(format!("address = \"{}\"", address));
    lines.push(format!("name = \"{}\"", name));
    
    let updated_content = lines.join("\n");
    fs::write("config.toml", updated_content)?;
    
    Ok(())
}

async fn remove_from_config_file(address: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string("config.toml")?;
    let lines: Vec<&str> = config_content.lines().collect();
    let mut new_lines: Vec<&str> = Vec::new();
    let mut i = 0;
    
    while i < lines.len() {
        let line = lines[i];
        
        // 檢查是否是錢包區塊的開始
        if line.trim() == "[[wallets]]" {
            // 查看下一行是否包含要刪除的地址
            let mut wallet_lines = vec![line];
            let mut j = i + 1;
            let mut found_target = false;
            
            // 收集這個錢包區塊的所有行
            while j < lines.len() {
                let next_line = lines[j];
                
                // 如果遇到下一個區塊或文件結束，停止收集
                if next_line.trim().starts_with("[") && next_line.trim() != "[[wallets]]" {
                    break;
                }
                
                // 如果遇到下一個錢包區塊，停止收集
                if next_line.trim() == "[[wallets]]" {
                    break;
                }
                
                wallet_lines.push(next_line);
                
                // 檢查是否包含目標地址
                if next_line.trim().starts_with("address = ") && next_line.contains(address) {
                    found_target = true;
                }
                
                j += 1;
                
                // 如果遇到空行且已經有了 name，這個錢包區塊結束
                if next_line.trim().is_empty() && wallet_lines.iter().any(|l| l.trim().starts_with("name = ")) {
                    break;
                }
            }
            
            // 如果不是目標錢包，保留這個區塊
            if !found_target {
                for wallet_line in wallet_lines {
                    new_lines.push(wallet_line);
                }
            }
            
            // 跳過已處理的行
            i = j;
        } else {
            // 保留非錢包區塊的行
            new_lines.push(line);
            i += 1;
        }
    }
    
    let updated_content = new_lines.join("\n");
    fs::write("config.toml", updated_content)?;
    
    Ok(())
}

// 創建gRPC流
async fn create_grpc_stream(
    grpc_endpoint: String,
    wallets: SharedWallets,
    db: SharedDatabase,
    restart_signal: GrpcRestartSignal,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        info!("🔄 嘗試連接到 gRPC 端點: {}", grpc_endpoint);
        
        match GeyserGrpcClient::build_from_shared(grpc_endpoint.clone()) {
            Ok(client_builder) => {
                match client_builder.connect().await {
                    Ok(mut client) => {
                        info!("✅ 成功連接到 gRPC 伺服器");
                        
                        let wallet_addresses: Vec<String> = {
                            let wallets_guard = wallets.lock().unwrap();
                            wallets_guard.keys().cloned().collect()
                        };
                        
                        info!("📋 準備訂閱 {} 個錢包:", wallet_addresses.len());
                        for (i, address) in wallet_addresses.iter().enumerate() {
                            let wallets_guard = wallets.lock().unwrap();
                            if let Some(wallet) = wallets_guard.get(address) {
                                info!("   {}: {} ({})", i + 1, wallet.name, &address[..8]);
                            } else {
                                info!("   {}: 未知錢包 ({})", i + 1, &address[..8]);
                            }
                        }
                        
                        // 計算所有錢包的 WSOL ATA 地址
                        let ata_addresses = calculate_all_wsol_atas(&wallet_addresses);
                        
                        // 創建 ATA 到錢包地址的映射
                        let mut ata_to_wallet_map: HashMap<String, String> = HashMap::new();
                        for (wallet_addr, ata_addr) in wallet_addresses.iter().zip(ata_addresses.iter()) {
                            ata_to_wallet_map.insert(ata_addr.clone(), wallet_addr.clone());
                        }
                        
                        info!("💎 準備監聽 {} 個 WSOL ATA 地址", ata_addresses.len());
                        
                        let mut accounts_filter = HashMap::new();
                        accounts_filter.insert(
                            "wallet_accounts".to_string(),
                            SubscribeRequestFilterAccounts {
                                account: wallet_addresses.clone(),
                                owner: vec![],
                                filters: vec![],
                                nonempty_txn_signature: None,
                            },
                        );
                        
                        // 監聽 WSOL ATA 地址
                        accounts_filter.insert(
                            "wsol_ata_accounts".to_string(),
                            SubscribeRequestFilterAccounts {
                                account: ata_addresses.clone(),
                                owner: vec![],
                                filters: vec![],
                                nonempty_txn_signature: None,
                            },
                        );

                        let request = SubscribeRequest {
                            accounts: accounts_filter,
                            slots: HashMap::new(),
                            transactions: HashMap::new(), // 不再監聽交易
                            transactions_status: HashMap::new(),
                            blocks: HashMap::new(),
                            blocks_meta: HashMap::new(),
                            entry: HashMap::new(),
                            commitment: Some(CommitmentLevel::Confirmed as i32),
                            accounts_data_slice: vec![],
                            ping: None,
                            from_slot: None,
                        };

                        match client.subscribe().await {
                            Ok((mut subscribe_tx, mut subscribe_rx)) => {
                                if let Err(e) = subscribe_tx.send(request).await {
                                    error!("❌ 發送訂閱請求失敗: {}", e);
                                    continue;
                                }
                                
                                info!("✅ gRPC 訂閱請求發送成功！");
                                info!("🎯 開始監聽 {} 個錢包的變化...", wallet_addresses.len());
                                
                                let mut first_message_received = false;
                                
                                while let Some(message) = subscribe_rx.next().await {
                                    // 檢查是否需要重啟
                                    {
                                        let mut signal = restart_signal.lock().unwrap();
                                        if *signal {
                                            *signal = false; // 重置信號
                                            info!("🔄 收到重啟信號，正在重新建立gRPC訂閱...");
                                            break; // 跳出內層循環，重新建立連接
                                        }
                                    }
                                    
                                    match message {
                                        Ok(update) => {
                                            if !first_message_received {
                                                info!("🎉 成功接收到第一個gRPC消息，訂閱正常工作！");
                                                first_message_received = true;
                                            }
                                            {
                                                let mut wallets_guard = wallets.lock().unwrap();
                                                
                                                // 只處理 Account 更新（SOL 和 WSOL）
                                                if let Some(UpdateOneof::Account(_)) = &update.update_oneof {
                                                    // 處理 SOL 帳戶更新
                                                    if let Err(e) = handle_sol_account_update(update.clone(), &mut wallets_guard, &wallet_addresses, &db) {
                                                        warn!("⚠️ 處理SOL帳戶更新時出錯: {}", e);
                                                    }
                                                    // 處理 WSOL ATA 帳戶更新
                                                    if let Err(e) = handle_wsol_account_update(update, &mut wallets_guard, &ata_to_wallet_map, &db) {
                                                        warn!("⚠️ 處理WSOL帳戶更新時出錯: {}", e);
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("❌ gRPC 流錯誤: {}", e);
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ 建立訂閱失敗: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("❌ 連接失敗: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("❌ 建立客戶端失敗: {}", e);
            }
        }
        
        warn!("⏳ 10秒後重新連接...");
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 載入配置
    let config = load_config()?;
    setup_logging(&config.logging.level);
    
    info!("🚀 SOL錢包監控器啟動");
    info!("📊 監控 {} 個錢包", config.wallets.len());
    
    // 初始化資料庫
    let database = match initialize_database() {
        Ok(db) => Arc::new(db),
        Err(e) => {
            error!("❌ 資料庫初始化失敗: {}", e);
            return Err(e);
        }
    };

    // 載入歷史資料
    let history_data = match load_all_wallet_history(&database) {
        Ok(data) => {
            info!("📖 成功載入歷史資料，包含 {} 個錢包的記錄", data.len());
            data
        }
        Err(e) => {
            warn!("⚠️ 載入歷史資料失敗: {}，將從空白開始", e);
            HashMap::new()
        }
    };

    // 初始化錢包追蹤器
    let mut wallets_map = HashMap::new();
    for wallet_config in &config.wallets {
        let mut wallet = WalletBalance::new(wallet_config.address.clone(), wallet_config.name.clone());
        
        // 從資料庫載入歷史數據（但不使用WSOL餘額，因為可能過時）
        if let Some(records) = history_data.get(&wallet_config.address) {
            info!("📚 為錢包 {} 載入 {} 條歷史記錄", wallet.name, records.len());
            wallet.load_history_from_db(records.clone());
        }
        
        wallets_map.insert(wallet_config.address.clone(), wallet);
    }
    
    // 所有錢包都需要從RPC獲取最新的SOL和WSOL餘額，確保數據準確性
    info!("🔄 正在從RPC獲取所有錢包的最新餘額...");
    initialize_wallets_from_rpc(&mut wallets_map, &database, &config.rpc.endpoint).await;
    
    let shared_wallets = Arc::new(Mutex::new(wallets_map));
    let grpc_restart_signal = Arc::new(Mutex::new(false));
    
    // 創建應用狀態
    let app_state = AppState {
        wallets: shared_wallets.clone(),
        database: database.clone(),
        grpc_restart_signal: grpc_restart_signal.clone(),
        config: config.clone(),
    };
    
    // 創建Web應用
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/wallets", get(get_wallets).post(add_wallet))
        .route("/api/wallets/:address", get(get_wallet_detail).delete(delete_wallet))
        .route("/api/chart", get(get_chart_data))
        .route("/ws", get(websocket_handler))
        .layer(CorsLayer::permissive())
        .with_state(app_state);
    
    // 啟動背景任務
    let grpc_wallets = shared_wallets.clone();
    let grpc_database = database.clone();
    let grpc_signal = grpc_restart_signal.clone();
    let grpc_endpoint = config.grpc.endpoint.clone();
    tokio::spawn(async move {
        if let Err(e) = create_grpc_stream(grpc_endpoint, grpc_wallets, grpc_database, grpc_signal).await {
            error!("❌ gRPC 流任務失敗: {}", e);
        }
    });
    
    // 移除定期WSOL更新任務，改為只從交易中更新WSOL
    
    // 啟動Web服務器
    let server_addr = format!("{}:{}", config.server.host, config.server.port);
    info!("🌐 Web服務器啟動於 http://{}", server_addr);
    
    let listener = tokio::net::TcpListener::bind(&server_addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}

