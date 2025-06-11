use {
    axum::{
        extract::{Path, Query, ws::{WebSocket, WebSocketUpgrade}},
        http::StatusCode,
        response::{Html, Response},
        routing::{get},
        Json, Router,
    },
    bs58,
    chrono::{DateTime, Utc},
    futures::{stream::StreamExt, sink::SinkExt},
    log::{error, info, warn},
    serde::{Deserialize, Serialize},
    std::{
        collections::{HashMap, VecDeque},
        fs,
        sync::{Arc, Mutex},
        time::Duration,
    },

    tower_http::cors::CorsLayer,
    yellowstone_grpc_client::GeyserGrpcClient,
    yellowstone_grpc_proto::{
        geyser::SubscribeUpdate,
        prelude::{
            CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
            SubscribeRequestFilterTransactions,
            subscribe_update::UpdateOneof,
        },
    },
};

// 常數定義
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const MAX_HISTORY_SIZE: usize = 10000;

// API 相關結構
#[derive(Debug, Serialize, Deserialize)]
struct WalletSummary {
    address: String,
    name: String,
    sol_balance: f64,
    wsol_balance: f64,
    total_balance: f64,
    last_update: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BalanceHistory {
    timestamp: DateTime<Utc>,
    sol_balance: f64,
    wsol_balance: f64,
    total_balance: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChartDataPoint {
    time: i64, // Unix timestamp in seconds
    value: f64,
}

#[derive(Debug, Deserialize)]
struct ChartQueryParams {
    wallet: String,
    data_type: String, // "sol", "wsol", or "total"
    interval: String,  // "5M", "10M", "30M", "1H", "2H", "4H", "8H", "12H", "1D", "1W", "ALL"
}

// 配置結構
#[derive(Debug, Deserialize)]
struct Config {
    grpc: GrpcConfig,
    wallets: Vec<WalletConfig>,
    logging: LoggingConfig,
    server: ServerConfig,
}

#[derive(Debug, Deserialize)]
struct GrpcConfig {
    endpoint: String,
}

#[derive(Debug, Deserialize)]
struct WalletConfig {
    address: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct LoggingConfig {
    level: String,
}

#[derive(Debug, Deserialize)]
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
        // 初始化時清空現有歷史並添加第一條記錄
        self.history.clear();
        self.add_to_history();
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

    fn to_summary(&self) -> WalletSummary {
        WalletSummary {
            address: self.address.clone(),
            name: self.name.clone(),
            sol_balance: self.sol_balance,
            wsol_balance: if self.wsol_initialized { self.wsol_balance } else { 0.0 },
            total_balance: self.total_balance(),
            last_update: self.last_update,
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


// Web API handlers
async fn get_wallets(wallets: axum::extract::State<SharedWallets>) -> Json<Vec<WalletSummary>> {
    let wallets_guard = wallets.lock().unwrap();
    let summaries: Vec<WalletSummary> = wallets_guard.values().map(|w| w.to_summary()).collect();
    Json(summaries)
}

async fn get_wallet_detail(
    Path(address): Path<String>,
    wallets: axum::extract::State<SharedWallets>,
) -> Result<Json<WalletSummary>, StatusCode> {
    let wallets_guard = wallets.lock().unwrap();
    match wallets_guard.get(&address) {
        Some(wallet) => Ok(Json(wallet.to_summary())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn get_chart_data(
    Query(params): Query<ChartQueryParams>,
    wallets: axum::extract::State<SharedWallets>,
) -> Result<Json<Vec<ChartDataPoint>>, StatusCode> {
    let wallets_guard = wallets.lock().unwrap();
    let wallet = wallets_guard.get(&params.wallet).ok_or(StatusCode::NOT_FOUND)?;
    
    let mut history: Vec<_> = wallet.history.iter().collect();
    
    // 根據時間間隔篩選數據
    let now = Utc::now();
    let filter_time = match params.interval.as_str() {
        "5M" => now - chrono::Duration::minutes(5),
        "10M" => now - chrono::Duration::minutes(10),
        "30M" => now - chrono::Duration::minutes(30),
        "1H" => now - chrono::Duration::hours(1),
        "2H" => now - chrono::Duration::hours(2),
        "4H" => now - chrono::Duration::hours(4),
        "8H" => now - chrono::Duration::hours(8),
        "12H" => now - chrono::Duration::hours(12),
        "1D" => now - chrono::Duration::days(1),
        "1W" => now - chrono::Duration::weeks(1),
        "ALL" => now - chrono::Duration::days(365),
        _ => now - chrono::Duration::hours(1),
    };
    
    history.retain(|h| h.timestamp >= filter_time);
    
    // 排序歷史數據以確保時間順序
    history.sort_by_key(|h| h.timestamp);
    
    let mut chart_data: Vec<ChartDataPoint> = history
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
    
    Ok(Json(chart_data))
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(wallets): axum::extract::State<SharedWallets>,
) -> Response {
    ws.on_upgrade(|socket| websocket_connection(socket, wallets))
}

async fn websocket_connection(mut socket: WebSocket, wallets: SharedWallets) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let summaries: Vec<WalletSummary> = {
                    let wallets_guard = wallets.lock().unwrap();
                    wallets_guard.values().map(|w| w.to_summary()).collect()
                };
                
                if let Err(_) = socket.send(axum::extract::ws::Message::Text(
                    serde_json::to_string(&summaries).unwrap_or_default()
                )).await {
                    break;
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

// 查詢錢包餘額 (初始化用)
async fn query_wallet_balance(wallet_address: &str) -> Result<(f64, f64), Box<dyn std::error::Error + Send + Sync>> {
    let rpc_url = "https://api.mainnet-beta.solana.com";
    let client = reqwest::Client::new();

    // 查詢 SOL 餘額
    let sol_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getBalance",
        "params": [wallet_address]
    });

    let sol_response = client
        .post(rpc_url)
        .json(&sol_request)
        .send()
        .await?;

    let sol_json: serde_json::Value = sol_response.json().await?;
    let sol_lamports = sol_json["result"]["value"].as_u64().unwrap_or(0);
    let sol_balance = sol_lamports as f64 / 1_000_000_000.0;

    // 查詢 WSOL 餘額
    let wsol_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTokenAccountsByOwner",
        "params": [
            wallet_address,
            { "mint": WSOL_MINT },
            { "encoding": "jsonParsed" }
        ]
    });

    let wsol_response = client
        .post(rpc_url)
        .json(&wsol_request)
        .send()
        .await?;

    let wsol_json: serde_json::Value = wsol_response.json().await?;
    let mut wsol_balance = 0.0;

    if let Some(accounts) = wsol_json["result"]["value"].as_array() {
        for account in accounts {
            if let Some(amount) = account["account"]["data"]["parsed"]["info"]["tokenAmount"]["uiAmount"].as_f64() {
                wsol_balance += amount;
            }
        }
    }

    Ok((sol_balance, wsol_balance))
}

// 初始化所有錢包餘額
async fn initialize_wallets(wallets: &mut HashMap<String, WalletBalance>) {
    let wallet_count = wallets.len();
    info!("🔄 開始初始化 {} 個錢包，每個錢包間隔15秒確保API穩定性", wallet_count);
    
    let mut processed_count = 0;
    
    for (address, wallet) in wallets.iter_mut() {
        processed_count += 1;
        info!("📋 正在初始化錢包 {}/{}: {} ({})", processed_count, wallet_count, wallet.name, &address[..8]);
        
        // 先初始化SOL餘額
        match query_wallet_balance(address).await {
            Ok((sol_balance, wsol_balance)) => {
                wallet.update_sol((sol_balance * 1_000_000_000.0) as u64);
                wallet.initialize_wsol(wsol_balance);
                info!("   📊 SOL: {:.6}, WSOL: {:.6}", sol_balance, wsol_balance);
            }
            Err(e) => {
                error!("❌ 初始化錢包 {} 的SOL和WSOL失敗: {}", wallet.name, e);
                // 即使查詢失敗，也要獲取WSOL餘額
                match query_wsol_balance(address).await {
                    Ok(wsol_balance) => {
                        wallet.initialize_wsol(wsol_balance);
                        info!("   📊 SOL查詢失敗，但成功獲取WSOL: {:.6}", wsol_balance);
                    }
                    Err(wsol_err) => {
                        error!("❌ 初始化錢包 {} 的WSOL也失敗: {}", wallet.name, wsol_err);
                        // 設置為0以避免未初始化狀態
                        wallet.initialize_wsol(0.0);
                    }
                }
            }
        }
        
        wallet.print_balance("初始化");
        
        // 等待15秒確保API穩定性
        if processed_count < wallet_count {
            info!("⏳ 等待15秒後初始化下一個錢包...");
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
        }
    }
    
    info!("✅ 錢包初始化完成！");
}



// 查詢WSOL餘額
async fn query_wsol_balance(wallet_address: &str) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
    let rpc_url = "https://api.mainnet-beta.solana.com";
    let client = reqwest::Client::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTokenAccountsByOwner",
        "params": [
            wallet_address,
            { "mint": WSOL_MINT },
            { "encoding": "jsonParsed" }
        ]
    });

    let response = client
        .post(rpc_url)
        .json(&request)
        .send()
        .await?;

    let json: serde_json::Value = response.json().await?;
    let mut total_balance = 0.0;

    if let Some(accounts) = json["result"]["value"].as_array() {
        for account in accounts {
            if let Some(amount) = account["account"]["data"]["parsed"]["info"]["tokenAmount"]["uiAmount"].as_f64() {
                total_balance += amount;
            }
        }
    }

    Ok(total_balance)
}

// 處理交易更新
fn handle_transaction_update(
    update: SubscribeUpdate,
    wallets: &mut HashMap<String, WalletBalance>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(UpdateOneof::Transaction(tx_update)) = update.update_oneof {
        if let Some(transaction) = tx_update.transaction {
            if let Some(meta) = &transaction.meta {
                let post_balances = &meta.post_balances;
                let post_token_balances = &meta.post_token_balances;
                let pre_token_balances = &meta.pre_token_balances;
                
                if let Some(tx) = &transaction.transaction {
                    if let Some(msg) = &tx.message {
                        // 處理SOL餘額變化
                        for (i, account_key) in msg.account_keys.iter().enumerate() {
                            let address = bs58::encode(account_key).into_string();
                            
                            if let Some(wallet) = wallets.get_mut(&address) {
                                if let Some(&balance) = post_balances.get(i) {
                                    let old_balance = wallet.sol_balance;
                                    wallet.update_sol(balance);
                                    
                                    if (wallet.sol_balance - old_balance).abs() > 0.000001 {
                                        wallet.print_balance("SOL交易");
                                    }
                                }
                            }
                        }
                        
                        // 處理WSOL（Token）餘額變化
                        info!("🪙 檢測到 {} 個 token 餘額變化", post_token_balances.len());
                        
                        for post_balance in post_token_balances {
                            if let Some(ui_token_amount) = &post_balance.ui_token_amount {
                                // 檢查是否為 WSOL 且屬於目標錢包
                                let is_wsol = post_balance.mint == WSOL_MINT;
                                let is_owner = wallets.contains_key(&post_balance.owner);
                                
                                if is_wsol && is_owner {
                                    let owner_address = &post_balance.owner;
                                    
                                    info!("🪙 Token變化: mint={}, owner={}, amount={}", 
                                          &post_balance.mint[..8],
                                          &owner_address[..8], 
                                          ui_token_amount.ui_amount);
                                    
                                    if let Some(wallet) = wallets.get_mut(owner_address) {
                                        // 查找對應的 pre balance
                                        let old_wsol_balance = pre_token_balances.iter()
                                            .find(|pre| pre.account_index == post_balance.account_index)
                                            .and_then(|pre| pre.ui_token_amount.as_ref())
                                            .map(|amount| amount.ui_amount)
                                            .unwrap_or(0.0);
                                        
                                        let new_wsol_balance = ui_token_amount.ui_amount;
                                        let wsol_balance_change = new_wsol_balance - old_wsol_balance;
                                        
                                        if wsol_balance_change.abs() > 0.000001 {
                                            info!("💎 錢包 {} WSOL 餘額變化: {:.9} SOL (從 {:.9} 到 {:.9})", 
                                                  &owner_address[..8], wsol_balance_change, old_wsol_balance, new_wsol_balance);
                                            
                                            wallet.update_wsol(new_wsol_balance);
                                            wallet.print_balance("WSOL交易");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// 創建gRPC流
async fn create_grpc_stream(
    grpc_endpoint: String,
    wallets: SharedWallets,
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

                        let mut transactions_filter = HashMap::new();
                        transactions_filter.insert(
                            "wallet_transactions".to_string(),
                            SubscribeRequestFilterTransactions {
                                vote: Some(false),
                                failed: Some(false),
                                signature: None,
                                account_include: wallet_addresses,
                                account_exclude: vec![],
                                account_required: vec![],
                            },
                        );

                        let request = SubscribeRequest {
                            accounts: accounts_filter,
                            slots: HashMap::new(),
                            transactions: transactions_filter,
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
                                
                                info!("🎯 開始監聽錢包變化...");
                                
                                while let Some(message) = subscribe_rx.next().await {
                                    match message {
                                        Ok(update) => {
                                            {
                                                let mut wallets_guard = wallets.lock().unwrap();
                                                if let Err(e) = handle_transaction_update(update, &mut wallets_guard) {
                                                    warn!("⚠️ 處理交易更新時出錯: {}", e);
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
    
    // 初始化錢包追蹤器
    let mut wallets_map = HashMap::new();
    for wallet_config in config.wallets {
        let wallet = WalletBalance::new(wallet_config.address.clone(), wallet_config.name);
        wallets_map.insert(wallet_config.address, wallet);
    }
    
    // 初始化錢包餘額
    initialize_wallets(&mut wallets_map).await;
    
    let shared_wallets = Arc::new(Mutex::new(wallets_map));
    
    // 創建Web應用
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/wallets", get(get_wallets))
        .route("/api/wallets/:address", get(get_wallet_detail))
        .route("/api/chart", get(get_chart_data))
        .route("/ws", get(websocket_handler))
        .layer(CorsLayer::permissive())
        .with_state(shared_wallets.clone());
    
    // 啟動背景任務
    let grpc_wallets = shared_wallets.clone();
    let grpc_endpoint = config.grpc.endpoint.clone();
    tokio::spawn(async move {
        if let Err(e) = create_grpc_stream(grpc_endpoint, grpc_wallets).await {
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
