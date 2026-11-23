-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Pure presentation model. Task credentials are never returned to the view.
local M = {}

local stages = {
    package_ready = "安装包已就绪", waiting_adaptation = "等待开发者适配",
    adapting = "正在适配", building = "正在构建", testing = "正在验证", failed = "适配失败",
}
local steps = {
    precheck = "检查安装环境", download = "下载安装包", verify = "校验安装包",
    extract = "解压安装包", guard = "核对设备型号", swap = "写入应用", done = "安装完成",
}
local feedback_categories = {
    works = "正常", white_screen = "白屏", touch = "触摸",
    layout = "布局", launch = "无法启动", other = "其它",
}
local feedback_page = "https://me.chen-cn.top/moyue/feedback.html"

-- Lua patterns treat "-" as a quantifier, so a literal page address has to be
-- escaped before it can be used as a pattern.
local function escape_pattern(text)
    return (text:gsub("([%.%-%+%*%?%[%]%^%$%(%)%%])", "%%%1"))
end

local function fields(text, header)
    local result = {}
    if not text or text:match("^([^\r\n]+)") ~= header then return result end
    for key, value in text:gmatch("\n([a-z_][a-z_0-9]*)=([^\r\n]*)") do
        if key ~= "key" then result[key] = value end
    end
    return result
end

local function safe(text)
    text = (text or ""):gsub("[%z\1-\31\127]", " ")
    local lower = text:lower()
    if lower:find("bearer", 1, true) or lower:find("key=", 1, true)
        or lower:find("password", 1, true) or lower:find("token", 1, true)
        or lower:find("authorization", 1, true) then return "" end
    return text
end

-- The feedback token never leaves this model: only the fact that a usable
-- link exists is handed to the view.
local function feedback_link(read_file, task_id)
    local text = read_file("feedback-link.txt")
    if not text then return false end
    if not text:find("MOYUE-FEEDBACK-LINK-1", 1, true) then return false end
    local url = text:match("url=([^\r\n]+)")
    if not url then return false end
    local id, token = url:match("^" .. escape_pattern(feedback_page) ..
        "#([0-9a-f]+)%.([0-9a-f]+)$")
    if not id or not token then return false end
    if #id ~= 32 or #token ~= 64 or id ~= task_id then return false end
    return true
end

function M.read(read_file, file_size, is_alive, installation_alive)
    local task = fields(read_file("task.txt"), "MOYUE-DEVICE-TASK-1")
    local status = fields(read_file("status.txt"), "MOYUE-DEVICE-STATUS-1")
    local install = fields(read_file("install.txt"), "MOYUE-DEVICE-INSTALL-1")
    local installed_app = fields(read_file("installed-app.txt"), "MOYUE-INSTALL-1")
    local action = fields(read_file("center-action.txt"), "MOYUE-CENTER-ACTION-1")
    local feedback = fields(read_file("feedback-result.txt"), "MOYUE-FEEDBACK-RESULT-1")
    local action_running = action.state == "running" and action.pid
        and action.pid:match("^%d+$") and (not is_alive or is_alive(action.pid)) or false
    local has_task = task.id and #task.id == 32 and not task.id:find("[^0-9a-f]") or false
    local feedback_modes = {
        feedback_link = true, feedback_send = true, feedback_logs = true,
    }
    local feedback_running = action_running and feedback_modes[action.mode] or false
    local model = {
        has_task = has_task, ready = has_task and status.state == "package_ready" or false,
        title = has_task and (stages[status.state] or "请求已提交") or "尚未上传设备信息",
        message = has_task and safe(status.message) or "上传设备信息后，服务器会匹配适用安装包。",
        version = safe(status.version), updated = safe(status.updated),
        build = (status.package or ""):match("build(%d+%-%d+)"),
        installing = install.state == "running" and (not installation_alive or installation_alive()),
        installed = install.state == "completed" and
            (not status.sha256 or status.sha256 == "" or installed_app.sha256 == status.sha256),
        action = action.mode, action_state = action.state,
        action_running = action_running,
        task_id = has_task and task.id:sub(1, 8) or nil,
        has_feedback_link = has_task and feedback_link(read_file, task.id),
        feedback_running = feedback_running,
        feedback_state = feedback.state or "none",
        feedback_kind = feedback.kind or "",
        feedback_receipt = safe(feedback.receipt or ""),
        feedback_message = safe(feedback.message or ""),
        feedback_category = feedback_categories[action.category or ""] or "",
    }
    if model.installing then
        model.title = steps[install.step] or "正在安装"
        local size = file_size("install/txn/package.tar.gz") or 0
        model.message = string.format("已下载 %.1f MB，可离开页面，稍后重新查看。", size / 1048576)
    elseif model.installed then
        model.title = "墨阅安装完成"
        model.message = "可从书库入口或 KUAL 启动墨阅。"
    elseif install.state == "running" then
        model.title = "安装已中断"
        model.message = "点击下载并安装，可恢复上次未完成的安装。"
    elseif feedback_running then
        local titles = {
            feedback_link = "正在取得反馈链接", feedback_send = "正在提交反馈",
            feedback_logs = "正在上传日志",
        }
        model.title = titles[action.mode] or "正在处理反馈"
        model.message = "反馈操作正在后台进行，可以离开页面，稍后重新查看。"
    elseif action_running then
        local titles = { submit = "正在上传设备信息", status = "正在刷新状态", install = "正在请求下载安装" }
        model.title = titles[action.mode] or "正在处理请求"
        model.message = "请求正在后台进行，可以离开页面，稍后重新查看。"
    elseif install.state == "failed" then
        model.title = "安装未完成"
        model.message = "请检查 Wi-Fi，再次点击下载并安装。已有日志和恢复记录已保留。"
    elseif action.state == "failed" and feedback_modes[action.mode] then
        model.title = "反馈没有成功"
        model.message = "请检查 Wi-Fi 后重试。"
    elseif action.state == "failed" then
        model.message = "上次请求未成功，请刷新状态或稍后重试。"
    elseif action.state == "running" then
        model.message = "上次请求已中断，请重试。"
    end
    model.ready = model.ready and not model.installing and not model.installed and not action_running
    return model
end

return M
