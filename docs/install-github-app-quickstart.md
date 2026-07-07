# GitHub App 安裝 — 一頁 Quick Start

給**第一次安裝**、不想先看完整技術文件的人。完整步驟見
[SOP：install-github-app.md](install-github-app.md)。

## 這是什麼？

裝好之後，GitHub 上的 Pull Request 可以自動（或手動）觸發 AI 評審團；
評審結果會以 **機器人帳號**（例如 `my-council[bot]`）發在 PR 留言裡。

你需要兩邊都設好：

1. **Zeabur 上的評審服務**（控制平面 + 三個 bot）
2. **GitHub 上的 App**（負責收 webhook、讓 chair 用 bot 身份發言）

---

## 開始前準備

| 項目 | 說明 |
|------|------|
| Zeabur 帳號 | 能建立專案、部署模板 |
| GitHub 權限 | 建立 App 需要帳號權限；裝到 **組織** 需要 **組織 Owner**（或「管理 GitHub Apps」權限） |
| 技術同事一位 | 執行終端機指令（或把本文轉給他） |
| 網域 | 例如 `my-council.zeabur.app`（模板部署時設定） |

請技術同事先準備好：`gh` 已登入、已 clone
[openab-control-plane](https://github.com/canyugs/openab-control-plane)  repo。

---

## 五步完成

### 步驟 1 — 在 Zeabur 部署評審團

在 Zeabur 用模板 **OpenAB Review Council (GitHub App)**（代碼 `1E1Y97`）部署。

記下兩個值（之後會用到）：

- **網址**：`https://<你的網域>.zeabur.app`
- **Webhook 密鑰**：部署時產生的 `GITHUB_WEBHOOK_SECRET`（一串隨機 hex）

等 `control-plane`、`chair`、`rev1`、`rev2` 四個服務都顯示運行中。

---

### 步驟 2 — 在 GitHub 建立 App

**建議：用網頁手動建立**（不用裝額外軟體）。

請有 GitHub 權限的人：

1. 打開終端機，執行（把網址和名稱改成你的）：

   ```sh
   scripts/install-github-app.sh manual \
     --plane-url https://<你的網域>.zeabur.app \
     --app-name "My Review Council" \
     --org <你的組織名>    # 個人帳號用的 App 可省略 --org
   ```

2. 依照畫面上印出的清單，在 GitHub 網頁填寫設定。
3. **一定要開的權限**：Pull requests（讀寫）、Contents（唯讀）、Commit statuses（讀寫）、**Issues（讀寫）**。
4. **一定要訂閱的事件**：Pull requests、Issue comments。
5. Webhook 網址填：`https://<你的網域>.zeabur.app/api/v1/github_webhooks`
6. Webhook 密鑰填：步驟 1 記下的那串。
7. 產生並**下載私鑰**（`.pem` 檔），記下 **App ID**。

> App 顯示名稱在全 GitHub 必須唯一。若被佔用，換一個名字即可（例如 `Acme PR Review`）。

---

### 步驟 3 — 把 App 裝到組織或 repo

```sh
scripts/install-github-app.sh install-url \
  --slug <App-的-slug> \
  --org <組織名>
```

打開印出的網址 → 選要安裝的 repo（建議先選 **All repositories** 或指定測試 repo）。

裝好後，請技術同事查 **Installation ID**：

```sh
scripts/install-github-app.sh list-installations \
  --app-id <App-ID> \
  --key-path <下載的.pem-路徑>
```

---

### 步驟 4 — 把 GitHub 和 Zeabur 接起來

請技術同事執行（數值都換成你的）：

```sh
scripts/install-github-app.sh wire \
  --app-id <App-ID> \
  --installation-id <Installation-ID> \
  --key-path <.pem-路徑> \
  --plane-url https://<你的網域>.zeabur.app \
  --webhook-secret <步驟1的密鑰> \
  --bot-handle <App-slug> \
  --chair-service-id <chair-服務-ID> \
  --plane-service-id <control-plane-服務-ID> \
  --server-id <專用伺服器-ID> \
  --chair-home /home/agent \
  --delivery zeabur-ssh
```

- 用 **Kiro** 當 agent 時，`--chair-home` 用 `/home/agent`
- 用預設 **Claude** 模板時，改成 `/home/node`
- 沒有專用伺服器時，把 `--delivery zeabur-ssh` 改成 `zeabur-exec` 並拿掉 `--server-id`

---

### 步驟 5 — 試一次

在已安裝 App 的 repo 開一個 PR，或在其留言打：

```text
/review
```

幾分鐘內應看到 bot 發「進行中」留言，之後更新為評審結論。

也可在 PR 留言 `@<App-slug> 這段程式碼有什麼風險？` 做追問。

---

## 完成了嗎？快速檢查

- [ ] PR 上出現 `<slug>[bot]` 的留言（不是你自己的帳號）
- [ ] `/review` 有反應
- [ ] GitHub App 設定裡 Recent deliveries 對 webhook 的回應是 **200**

---

## 常見問題

| 狀況 | 怎麼辦 |
|------|--------|
| 建立 App 時說 `issue_comment` 權限錯誤 | 補上 **Issues：讀寫** |
| App 名稱已被使用 | 換一個顯示名稱 |
| `/review` 沒反應 | 確認 App 已安裝在該 repo；留言者要是成員以上；等 1–2 分鐘 |
| 留言出現在自己帳號名下 | 請技術同事重跑步驟 4；確認 chair 沒有舊的 `GH_TOKEN` |

---

## 需要更多細節？

- 完整 SOP（含本地開發、manifest 流程、故障排除）：[install-github-app.md](install-github-app.md)
- 指令總覽：`scripts/install-github-app.sh help`
- 填寫用工作表：`scripts/install-github-app.sh worksheet`