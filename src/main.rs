use {
    bs58,
    chrono::{DateTime, Utc},
    futures::{stream::StreamExt, sink::SinkExt},
    log::{error, info, warn},
    serde::Deserialize,
    std::{
        collections::HashMap,
        fs,
    },
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

// 配置結構
#[derive(Debug, Deserialize)]
struct Config {
    grpc: GrpcConfig,
    wallets: Vec<WalletConfig>,
    logging: LoggingConfig,
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

// 錢包餘額追蹤器
#[derive(Debug, Clone)]
struct WalletBalance {
    address: String,
    name: String,
    sol_balance: f64,
    wsol_balance: f64,
    wsol_initialized: bool,  // 新增：標記WSOL是否已初始化
    last_update: DateTime<Utc>,
}

impl WalletBalance {
    fn new(address: String, name: String) -> Self {
        Self {
            address,
            name,
            sol_balance: 0.0,
            wsol_balance: 0.0,
            wsol_initialized: false,  // 預設為未初始化
            last_update: Utc::now(),
        }
    }

    fn update_sol(&mut self, lamports: u64) {
        self.sol_balance = lamports as f64 / 1_000_000_000.0;
        self.last_update = Utc::now();
    }

    fn update_wsol(&mut self, amount: f64) {
        self.wsol_balance = amount;
        self.wsol_initialized = true;  // 更新時標記為已初始化
        self.last_update = Utc::now();
    }

    fn initialize_wsol(&mut self, amount: f64) {
        self.wsol_balance = amount;
        self.wsol_initialized = true;
        self.last_update = Utc::now();
    }

    fn total_balance(&self) -> f64 {
        // 如果WSOL未初始化，只計算SOL餘額
        if !self.wsol_initialized {
            self.sol_balance
        } else {
            self.sol_balance + self.wsol_balance
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

// 讀取配置檔案
fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&config_content)?;
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
    info!("🔄 開始初始化 {} 個錢包，每個錢包間隔5秒避免API速率限制", wallet_count);
    
    let mut processed_count = 0;
    
    for (address, wallet) in wallets.iter_mut() {
        processed_count += 1;
        info!("📋 正在初始化錢包 {}/{}: {} ({})", processed_count, wallet_count, wallet.name, &address[..8]);
        
        // 先查詢SOL餘額（通常比較穩定）
        match query_wallet_balance(address).await {
            Ok((sol_balance, _)) => {
                wallet.sol_balance = sol_balance;
                wallet.last_update = Utc::now();
                info!("✅ {} SOL 餘額初始化成功: {:.6}", wallet.name, sol_balance);
            }
            Err(e) => {
                error!("❌ 查詢錢包 {} SOL 餘額失敗: {}", &address[..8], e);
            }
        }

        // 添加1秒延遲，避免SOL和WSOL查詢過於密集
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // 專門處理WSOL初始化，最多重試3次
        let mut wsol_initialized = false;
        for attempt in 1..=3 {
            info!("🔄 正在嘗試初始化 {} 的 WSOL 餘額 (第 {} 次)", wallet.name, attempt);
            
            match query_wsol_balance(address).await {
                Ok(wsol_balance) => {
                    wallet.initialize_wsol(wsol_balance);
                    info!("✅ {} WSOL 餘額初始化成功: {:.6}", wallet.name, wsol_balance);
                    wsol_initialized = true;
                    break;
                }
                Err(e) => {
                    warn!("⚠️ 第 {} 次查詢 {} WSOL 餘額失敗: {}", attempt, wallet.name, e);
                    if attempt < 3 {
                        info!("⏳ 等待 15 秒後重試...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                    }
                }
            }
        }
        
        if !wsol_initialized {
            warn!("❌ {} WSOL 餘額初始化失敗，標記為未初始化", wallet.name);
        }

        // 印出初始化結果
        wallet.print_balance("初始化");
        
        // 如果不是最後一個錢包，等待5秒再處理下一個
        if processed_count < wallet_count {
            info!("⏳ 等待 15 秒後初始化下一個錢包...");
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
        }
    }
    
    info!("🎉 所有錢包初始化完成！");
}

// 專門查詢WSOL餘額的函數
async fn query_wsol_balance(wallet_address: &str) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
    let rpc_url = "https://api.mainnet-beta.solana.com";
    let client = reqwest::Client::new();

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
    
    // 檢查是否有錯誤響應
    if let Some(error) = wsol_json.get("error") {
        let error_code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        let error_message = error.get("message").and_then(|m| m.as_str()).unwrap_or("未知錯誤");
        
        if error_code == 429 {
            return Err(format!("API速率限制: {}", error_message).into());
        } else {
            return Err(format!("RPC錯誤 {}: {}", error_code, error_message).into());
        }
    }
    
    let mut wsol_balance = 0.0;

    if let Some(accounts) = wsol_json["result"]["value"].as_array() {
        for account in accounts {
            if let Some(amount) = account["account"]["data"]["parsed"]["info"]["tokenAmount"]["uiAmount"].as_f64() {
                wsol_balance += amount;
            }
        }
    }

    Ok(wsol_balance)
}

// 處理交易更新
async fn handle_transaction_update(
    update: SubscribeUpdate,
    wallets: &mut HashMap<String, WalletBalance>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(UpdateOneof::Transaction(transaction_update)) = update.update_oneof {
        let transaction = transaction_update.transaction.unwrap();
        let signature = bs58::encode(&transaction.signature).into_string();
        
        info!("🔄 收到交易: {}", signature);

        if let Some(meta) = &transaction.meta {
            let pre_balances = &meta.pre_balances;
            let post_balances = &meta.post_balances;
            
            // 獲取帳戶列表
            let account_keys: Vec<String> = if let Some(tx) = &transaction.transaction {
                if let Some(msg) = &tx.message {
                    msg.account_keys.iter().map(|key| bs58::encode(key).into_string()).collect()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            // 處理每個監控的錢包
            for (wallet_address, wallet) in wallets.iter_mut() {
                let mut balance_changed = false;

                // 檢查 SOL 餘額變化
                if let Some(wallet_index) = account_keys.iter().position(|key| key == wallet_address) {
                    if wallet_index < pre_balances.len() && wallet_index < post_balances.len() {
                        let pre_balance = pre_balances[wallet_index];
                        let post_balance = post_balances[wallet_index];
                        let sol_change = (post_balance as i64 - pre_balance as i64) as f64 / 1_000_000_000.0;
                        
                        if sol_change.abs() > 0.0 {
                            wallet.update_sol(post_balance);
                            balance_changed = true;
                            info!("💰 {} SOL 變化: {:.9}", &wallet.name, sol_change);
                        }
                    }
                }

                // 檢查 WSOL 餘額變化
                for post_balance in &meta.post_token_balances {
                    if let Some(token_balance) = &post_balance.ui_token_amount {
                        let is_wsol = post_balance.mint == WSOL_MINT;
                        let is_owner = post_balance.owner == *wallet_address;
                        
                        if is_wsol && is_owner {
                            let new_wsol_balance = token_balance.ui_amount;
                            
                            let old_wsol_balance = meta.pre_token_balances.iter()
                                .find(|pre| pre.account_index == post_balance.account_index)
                                .and_then(|pre| pre.ui_token_amount.as_ref())
                                .map(|amount| amount.ui_amount)
                                .unwrap_or(0.0);
                            
                            let wsol_change = new_wsol_balance - old_wsol_balance;
                            
                            if wsol_change.abs() > 0.0 {
                                // 如果WSOL尚未初始化，透過交易初始化
                                if !wallet.wsol_initialized {
                                    wallet.initialize_wsol(new_wsol_balance);
                                    info!("💎 {} WSOL 透過交易初始化: {:.9}", &wallet.name, new_wsol_balance);
                                } else {
                                    wallet.update_wsol(new_wsol_balance);
                                    info!("💎 {} WSOL 變化: {:.9}", &wallet.name, wsol_change);
                                }
                                balance_changed = true;
                            }
                        }
                    }
                }

                // 如果有餘額變化，印出更新後的狀態
                // 但如果只是WSOL未初始化的狀態更新，則不印出（避免影響總額顯示）
                if balance_changed {
                    wallet.print_balance("餘額更新");
                }
            }
        }
    }

    Ok(())
}

// 建立 gRPC 串流監聽
async fn create_grpc_stream(
    grpc_endpoint: String,
    wallets: &mut HashMap<String, WalletBalance>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("🔗 連接到 gRPC 端點: {}", grpc_endpoint);
    
    let mut client = GeyserGrpcClient::build_from_shared(grpc_endpoint)?
        .connect()
        .await?;
    info!("✅ 成功連接到 gRPC 服務器");

    // 初始化所有錢包餘額
    initialize_wallets(wallets).await;

    let monitored_addresses: Vec<String> = wallets.keys().cloned().collect();
    info!("📋 開始監控 {} 個錢包", monitored_addresses.len());

    // 設定交易過濾器
    let transaction_filter = SubscribeRequestFilterTransactions {
        vote: Some(false),
        failed: Some(false),
        signature: None,
        account_include: monitored_addresses.clone(),
        account_exclude: vec![],
        account_required: vec![],
    };

    // 設定帳戶過濾器
    let accounts_filter = SubscribeRequestFilterAccounts {
        account: monitored_addresses.clone(),
        owner: monitored_addresses.clone(),
        filters: vec![],
        nonempty_txn_signature: Some(false),
    };

    let request = SubscribeRequest {
        accounts: HashMap::from([("accounts".to_string(), accounts_filter)]),
        slots: HashMap::new(),
        transactions: HashMap::from([("transactions".to_string(), transaction_filter)]),
        transactions_status: HashMap::new(),
        blocks: HashMap::new(),
        blocks_meta: HashMap::new(),
        entry: HashMap::new(),
        commitment: Some(CommitmentLevel::Processed as i32),
        accounts_data_slice: vec![],
        ping: None,
        from_slot: None,
    };

    let (mut subscribe_tx, mut subscribe_rx) = client.subscribe().await?;
    subscribe_tx.send(request).await?;
    info!("🚀 成功建立訂閱，開始監聽交易...");
    
    // 處理訂閱響應
    while let Some(message) = subscribe_rx.next().await {
        match message {
            Ok(update) => {
                if let Err(e) = handle_transaction_update(update, wallets).await {
                    error!("❌ 處理更新時發生錯誤: {}", e);
                }
            }
            Err(e) => {
                error!("❌ gRPC 串流錯誤: {}", e);
                break;
            }
        }
    }
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 載入配置
    let config = load_config()?;
    
    // 設定日誌
    setup_logging(&config.logging.level);
    
    info!("🚀 SOL 錢包監控程式啟動");
    info!("📍 gRPC 端點: {}", config.grpc.endpoint);
    
    // 初始化錢包列表
    let mut wallets: HashMap<String, WalletBalance> = HashMap::new();
    for wallet_config in &config.wallets {
        let wallet = WalletBalance::new(
            wallet_config.address.clone(),
            wallet_config.name.clone()
        );
        wallets.insert(wallet_config.address.clone(), wallet);
        info!("📝 已加入監控錢包: {} ({})", wallet_config.name, &wallet_config.address[..8]);
    }
    
    if wallets.is_empty() {
        error!("❌ 沒有配置錢包地址，請檢查 config.toml");
        return Ok(());
    }
    
    // 啟動監聽
    loop {
        match create_grpc_stream(config.grpc.endpoint.clone(), &mut wallets).await {
            Ok(_) => {
                info!("ℹ️ gRPC 串流正常結束");
            }
            Err(e) => {
                error!("❌ gRPC 串流錯誤: {}", e);
            }
        }
        
        warn!("🔄 連接斷開，5秒後重新連接...");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}
