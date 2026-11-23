-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Dedicated adaptation UI using the installed KOReader display/input runtime.
io.stdout:setvbuf("line")
os.setlocale("C", "numeric")
require("setupkoenv")
G_defaults = require("luadefaults"):open()
G_reader_settings = require("luasettings"):wrap({ language = "zh_CN" })
require("gettext").changeLang("zh_CN")

local Device = require("device")
require("document/canvascontext"):init(Device)
require("ui/bidi").setup("zh_CN")
local UIManager = require("ui/uimanager")
local InputContainer = require("ui/widget/container/inputcontainer")
local CenterContainer = require("ui/widget/container/centercontainer")
local FrameContainer = require("ui/widget/container/framecontainer")
local VerticalGroup = require("ui/widget/verticalgroup")
local VerticalSpan = require("ui/widget/verticalspan")
local TextWidget = require("ui/widget/textwidget")
local TextBoxWidget = require("ui/widget/textboxwidget")
local ButtonTable = require("ui/widget/buttontable")
local ConfirmBox = require("ui/widget/confirmbox")
local QRWidget = require("ui/widget/qrwidget")
local Font = require("ui/font")
local Geom = require("ui/geometry")
local Blitbuffer = require("ffi/blitbuffer")
local lfs = require("libs/libkoreader-lfs")
local Screen = Device.screen
local home = assert(os.getenv("MOYUE_CENTER_HOME"))
local Model = dofile(home .. "/center-model.lua")
local output = "/mnt/us/moyue-device-probe/"

local feedback_page = "https://me.chen-cn.top/moyue/feedback.html"

-- Lua patterns treat "-" as a quantifier, so a literal page address has to be
-- escaped before it can be used as a pattern.
local function escape_pattern(text)
    return (text:gsub("([%.%-%+%*%?%[%]%^%$%(%)%%])", "%%%1"))
end

local function read_file(name)
    local path = name == "installed-app.txt" and "/mnt/us/aristotle/.moyue-install" or output .. name
    if lfs.symlinkattributes(path, "mode") ~= "file" then return nil end
    local file = io.open(path, "rb")
    if not file then return nil end
    local text = file:read(32768)
    file:close()
    return text
end

local function file_size(name)
    if lfs.symlinkattributes(output .. name, "mode") ~= "file" then return nil end
    return lfs.attributes(output .. name, "size")
end

local function shell_quote(text)
    return "'" .. text:gsub("'", "'\\''") .. "'"
end

local function owned_process(pid, script)
    if not pid or not pid:match("^%d+$") then return false end
    local file = io.open("/proc/" .. pid .. "/cmdline", "rb")
    if not file then return false end
    local command = file:read(4096) or ""
    file:close()
    return command:find(home .. "/" .. script, 1, true) ~= nil
end

-- The feedback URL carries a feedback-only token. It is never printed, only
-- drawn as a QR code for the phone.
local function feedback_url()
    local text = read_file("feedback-link.txt")
    if not text then return nil end
    if not text:find("MOYUE-FEEDBACK-LINK-1", 1, true) then return nil end
    local url = text:match("url=([^\r\n]+)")
    if not url then return nil end
    local id, token = url:match("^" .. escape_pattern(feedback_page) ..
        "#([0-9a-f]+)%.([0-9a-f]+)$")
    if not id or not token then return nil end
    if #id ~= 32 or #token ~= 64 then return nil end
    local task = read_file("task.txt") or ""
    if task:match("\nid=([0-9a-f]+)\n") ~= id then return nil end
    return url
end

local function snapshot()
    return Model.read(read_file, file_size, function(pid)
        return owned_process(pid, "center-action.sh")
    end, function()
        local file = io.open("/var/tmp/moyue-device-install.lock/pid", "rb")
        if not file then return false end
        local pid = (file:read(32) or ""):match("^(%d+)")
        file:close()
        return owned_process(pid, "install.sh")
    end)
end

local Center = InputContainer:extend{}

function Center:request(mode, category)
    if self.busy then return end
    self.busy = mode
    self.busy_ticks = 0
    local command = "setsid sh " .. shell_quote(home .. "/center-action.sh") .. " " .. mode
    if category then command = command .. " " .. shell_quote(category) end
    command = command .. " </dev/null >/dev/null 2>&1 &"
    os.execute(command)
    self:rebuild(true)
end

function Center:confirm(mode, category)
    local text = mode == "submit"
        and "同意上传设备型号、固件和硬件属性到开发者 Oracle 服务器，用于匹配安装包？"
        or "下载并安装服务器为本机匹配的墨阅安装包？安装后可返回 KUAL 启动墨阅。"
    local ok_text = mode == "submit" and "同意上传" or "下载并安装"
    if mode == "feedback_logs" then
        text = "确认上传日志？日志只含墨阅应用与适配中心的脱敏尾部，" ..
            "不含书正文、账号信息和序列号。"
        ok_text = "确认上传"
    end
    UIManager:show(ConfirmBox:new{
        text = text, ok_text = ok_text,
        cancel_text = "取消", ok_callback = function() self:request(mode, category) end,
    })
end

function Center:feedback(category)
    if self.busy then return end
    local model = self.model or {}
    if not model.has_feedback_link then
        -- The contract wants a feedback token first: fetch it, then send.
        self.pending_category = category
        self:request("feedback_link")
        return
    end
    self:request("feedback_send", category)
end

function Center:show_feedback()
    self.view = "feedback"
    self:rebuild(true)
end

function Center:show_main()
    self.view = "main"
    self:rebuild(true)
end

function Center:build_main(model, busy, busy_label)
    local title = self.busy and busy_label[self.busy] or model.title
    local report = read_file("report.txt") or ""
    local code = report:match("device code: ([%w]+) %(") or "待采集"
    local firmware = report:match("prettyversion: Kindle ([%d.]+)") or "待采集"
    local device_name = Device.model:gsub("KindlePaperWhite4", "Kindle Paperwhite 4"):gsub("KindleOasis2", "Kindle Oasis 2")
    local lines = { title, "", model.message }
    if model.version ~= "" then
        local build = model.build and (" / 构建 " .. model.build) or ""
        table.insert(lines, "\n可用版本  " .. model.version .. build)
    end
    if model.updated ~= "" then table.insert(lines, "更新时间  " .. model.updated:gsub("T", " "):gsub("Z", " UTC")) end
    if model.task_id then table.insert(lines, "任务编号  " .. model.task_id) end
    return {
        lines = table.concat(lines, "\n"),
        device = device_name .. " (" .. code .. ")   固件 " .. firmware,
    }
end

function Center:build_feedback(model, busy)
    local note = "这里反馈的是墨阅试装版本（预览版或试用版）的问题。" ..
        "白屏、触摸错位、排版异常和无法启动，请选对应分类提交。"
    local lines = { note, "" }
    if model.feedback_running then
        table.insert(lines, "反馈正在后台提交，可以离开页面，稍后回来查看。")
    elseif model.feedback_state == "completed" then
        local kind = {
            link = "反馈链接已取得", send = "反馈已提交",
            logs = "日志已上传",
        }
        table.insert(lines, (kind[model.feedback_kind] or "操作已完成") ..
            (model.feedback_receipt ~= "" and ("，回执 " .. model.feedback_receipt:sub(1, 8)) or ""))
    elseif model.feedback_state == "failed" then
        table.insert(lines, "上次反馈没有成功：" .. model.feedback_message)
    else
        table.insert(lines, "还没有提交过反馈。")
    end
    if not model.has_feedback_link then
        table.insert(lines, "\n第一次提交会先取得反馈链接，再把反馈发出去。")
    end
    local body = table.concat(lines, "\n")
    local can_send = not busy and not model.feedback_running
    return {
        lines = body,
        rows = {
            {
                { text = "正常", enabled = can_send,
                  height = self.button_height, callback = function() self:feedback("works") end },
                { text = "白屏", enabled = can_send,
                  height = self.button_height, callback = function() self:feedback("white_screen") end },
            },
            {
                { text = "触摸", enabled = can_send,
                  height = self.button_height, callback = function() self:feedback("touch") end },
                { text = "布局", enabled = can_send,
                  height = self.button_height, callback = function() self:feedback("layout") end },
            },
            {
                { text = "无法启动", enabled = can_send,
                  height = self.button_height, callback = function() self:feedback("launch") end },
                { text = "其它", enabled = can_send,
                  height = self.button_height, callback = function() self:feedback("other") end },
            },
            {
                { text = "上传日志", enabled = not busy and model.has_task,
                  height = self.button_height, callback = function() self:confirm("feedback_logs") end },
                { text = "扫码反馈", enabled = not busy and model.has_task,
                  height = self.button_height, callback = function() self:request("feedback_link") end },
            },
            {
                { text = "返回适配中心", height = self.button_height,
                  callback = function() self:show_main() end },
            },
        },
    }
end

function Center:build_qr(model)
    local url = feedback_url()
    local size = math.floor(math.min(Screen:getWidth(), Screen:getHeight()) * 0.55)
    local widget = nil
    if url then
        widget = QRWidget:new{ text = url, width = size, height = size }
    end
    return {
        url = url,
        widget = widget,
        lines = "用手机相机扫码，填写问题详情。",
    }
end

function Center:rebuild(force)
    local model = snapshot()
    local busy = self.busy or model.action_running
    local busy_label = {
        submit = "正在上传设备信息…", status = "正在刷新状态…", install = "正在请求下载安装…",
        feedback_link = "正在取得反馈链接…", feedback_send = "正在提交反馈…",
        feedback_logs = "正在上传日志…",
    }
    local fingerprint = table.concat({
        self.view or "main", tostring(self.busy), tostring(busy),
        tostring(model.title), tostring(model.message), tostring(model.version),
        tostring(model.updated), tostring(model.task_id), tostring(model.installing),
        tostring(model.installed), tostring(model.ready), tostring(model.action_state),
        tostring(model.has_feedback_link), tostring(model.feedback_state),
        tostring(model.feedback_kind), tostring(model.feedback_receipt),
        tostring(model.feedback_message), tostring(model.feedback_running),
    }, "|")
    if not force and fingerprint == self.fingerprint then return end
    self.fingerprint = fingerprint
    self.model = model
    local width, height = Screen:getWidth(), Screen:getHeight()
    local padding = Screen:scaleBySize(22)
    local content_width = width - padding * 2
    local button_height = Screen:scaleBySize(54)
    self.button_height = button_height
    local content
    if self.view == "feedback" then
        local page = self:build_feedback(model, busy)
        content = VerticalGroup:new{
            align = "left",
            TextWidget:new{ text = "试装验证反馈", face = Font:getFace("tfont", 28) },
            VerticalSpan:new{ width = Screen:scaleBySize(14) },
            TextBoxWidget:new{
                text = page.lines, width = content_width, height = math.floor(height * 0.22),
                face = Font:getFace("cfont", 21), height_overflow_show_ellipsis = true,
            },
            VerticalSpan:new{ width = Screen:scaleBySize(12) },
            ButtonTable:new{ width = content_width, buttons = page.rows },
        }
    elseif self.view == "qr" then
        local page = self:build_qr(model)
        local middle
        if page.widget then
            middle = page.widget
        else
            middle = TextWidget:new{
                text = "还没有取得反馈链接，请返回后重试。",
                face = Font:getFace("cfont", 21),
            }
        end
        content = VerticalGroup:new{
            align = "center",
            TextWidget:new{ text = "手机填写反馈", face = Font:getFace("tfont", 28) },
            VerticalSpan:new{ width = Screen:scaleBySize(10) },
            TextBoxWidget:new{
                text = page.lines, width = content_width, height = math.floor(height * 0.13),
                face = Font:getFace("cfont", 18), height_overflow_show_ellipsis = true,
            },
            VerticalSpan:new{ width = Screen:scaleBySize(10) },
            middle,
            VerticalSpan:new{ width = Screen:scaleBySize(10) },
            ButtonTable:new{
                width = content_width, buttons = {
                    {
                        { text = "返回适配中心", height = button_height,
                          callback = function() self:show_main() end },
                    },
                },
            },
        }
    else
        local page = self:build_main(model, busy, busy_label)
        content = VerticalGroup:new{
            align = "left",
            TextWidget:new{ text = "墨阅适配中心", face = Font:getFace("tfont", 28) },
            VerticalSpan:new{ width = Screen:scaleBySize(14) },
            TextWidget:new{ text = page.device, face = Font:getFace("cfont", 17) },
            VerticalSpan:new{ width = Screen:scaleBySize(24) },
            TextBoxWidget:new{
                text = page.lines, width = content_width, height = math.floor(height * 0.30),
                face = Font:getFace("cfont", 21), height_overflow_show_ellipsis = true,
            },
            VerticalSpan:new{ width = Screen:scaleBySize(12) },
            ButtonTable:new{
                width = content_width, buttons = {
                    {
                        { text = "刷新状态", enabled = not busy,
                          height = button_height, callback = function()
                              if model.has_task then self:request("status") else self:rebuild(true) end
                          end },
                        { text = "上传并请求适配", enabled = not busy and not model.installing,
                          height = button_height, callback = function() self:confirm("submit") end },
                    },
                    {
                        { text = model.installed and "墨阅已安装" or "下载并安装匹配包",
                          enabled = not busy and model.ready,
                          height = button_height, callback = function() self:confirm("install") end },
                    },
                    {
                        { text = "反馈与日志", enabled = not busy,
                          height = button_height, callback = function() self:show_feedback() end },
                    },
                    {
                        { text = "退出适配中心", height = button_height,
                          callback = function() self.closed = true; UIManager:quit() end },
                    },
                },
            },
        }
    end
    self.dimen = Geom:new{ x = 0, y = 0, w = width, h = height }
    if self[1] then self[1]:free() end
    self[1] = FrameContainer:new{
        width = width, height = height, background = Blitbuffer.COLOR_WHITE,
        bordersize = 0, padding = 0,
        CenterContainer:new{ dimen = self.dimen, content },
    }
    UIManager:setDirty(self, "ui")
end

function Center:tick()
    if self.closed then return end
    self.ticks = self.ticks + 1
    local model = snapshot()
    if self.busy then
        self.busy_ticks = self.busy_ticks + 1
        if self.busy_ticks > 1 and model.action == self.busy and model.action_state ~= "running" then
            local previous = self.busy
            self.busy = nil
            self.next_refresh = self.ticks + 15
            if previous == "submit" and model.action_state == "completed" and model.has_task then self:request("status") end
            if previous == "feedback_link" and model.action_state == "completed" and model.has_feedback_link then
                local pending = self.pending_category
                self.pending_category = nil
                if pending then
                    self:request("feedback_send", pending)
                else
                    self.view = "qr"
                end
            end
        elseif self.busy_ticks > 90 then
            self.busy = nil
            self.next_refresh = self.ticks + 15
        end
    elseif self.ticks >= self.next_refresh and model.has_task and not model.action_running then
        self.next_refresh = self.ticks + 15
        self:request("status")
    end
    self:rebuild()
    UIManager:scheduleIn(1, self.poll)
end

local center = Center:new{}
center.ticks = 0
center.next_refresh = 15
center.view = "main"
center.poll = function() center:tick() end
local ok, err = xpcall(function()
    center:rebuild(true)
    UIManager:show(center)
    UIManager:scheduleIn(1, center.poll)
    UIManager:run()
end, debug.traceback)
Device:exit()
if not ok then error(err) end
