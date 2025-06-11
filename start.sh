#!/bin/bash

# SOL 錢包監控器啟動腳本

echo "🚀 SOL 錢包監控器啟動中..."

# 檢查是否已編譯
if [ ! -f "./target/release/sol-wallet-monitor" ]; then
    echo "📦 正在編譯程式..."
    cargo build --release
    if [ $? -ne 0 ]; then
        echo "❌ 編譯失敗！"
        exit 1
    fi
    echo "✅ 編譯完成！"
fi

# 檢查配置文件
if [ ! -f "config.toml" ]; then
    echo "❌ 找不到 config.toml 文件！"
    echo "請確保配置文件存在並已正確設定。"
    exit 1
fi

# 顯示配置信息
echo "📋 當前配置："
echo "   - gRPC 端點: $(grep endpoint config.toml | cut -d '"' -f 2)"
echo "   - Web 服務器: $(grep host config.toml | cut -d '"' -f 2):$(grep port config.toml | cut -d '=' -f 2 | tr -d ' ')"
echo "   - 監控錢包數量: $(grep -c '\[\[wallets\]\]' config.toml)"

echo ""
echo "🌐 啟動 Web 服務器..."
echo "   前端界面: http://127.0.0.1:3000"
echo "   API 端點: http://127.0.0.1:3000/api/wallets"
echo ""
echo "按 Ctrl+C 停止服務器"
echo "======================================"

# 啟動程式
./target/release/sol-wallet-monitor 