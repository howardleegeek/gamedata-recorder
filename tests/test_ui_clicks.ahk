#Requires AutoHotkey >=2.0

InitInputSettings() {
    SetDefaultMouseSpeed 0  ; Instant mouse movements. Gotta go fast
    SendMode "Event"    ;   low level handler for precise timing
    SetKeyDelay 0, 0    ; no keystroke delay
}

InitUIVars() {
    global
    reset_position := Array(376, 30)  ;   Origin within the titlebar of `Account`
    ;   Offsets for rearranging elements
    collapse_offset := -160
    option_offset := 28  ; offset for y coordinate between options, if stop_toggle is on, etc

    /* Hex RBG color of boolean elements at a bright(TRUE) state.
    Deeply coupled with their coordinates. Trial run to ensure accurate reflection of state */
    checked_true := 0xADB5D3
    collapse_true := 0x13151A

    /* UI elements in order, categorized by layout header
    Format: Array(X coord, Y coord, option count)
    Windows Resolution : 1920x1080
    DPI scale : 125%
    Todo: scale XY with DPI */

    ;   Account
    logout_button := Array(696, 98)
    if (logout_button.Length < 2) {
        MsgBox("logout_button array must have at least 2 elements")
        ExitApp()
    }

    ;   Keyboard Shortcuts
    hotkey_button := Array(412, 186)
    hotkey_stop := Array(412, 216)
    if (hotkey_stop.Length < 2) {
        MsgBox("hotkey_stop array must have at least 2 elements")
        ExitApp()
    }
    stop_button := Array(225, 223)
    if (stop_button.Length < 2) {
        MsgBox("stop_button array must have at least 2 elements")
        ExitApp()
    }
    if (stop_button[1] < 0 || stop_button[1] > 1920) {
        MsgBox("stop_button X coordinate out of bounds: " . stop_button[1])
        ExitApp()
    }
    if (stop_button[2] < 0 || stop_button[2] > 1200) {
        MsgBox("stop_button Y coordinate out of bounds: " . stop_button[2])
        ExitApp()
    }
    stop_toggle := PixelGetColor(stop_button[1], stop_button[2])

    ;   Recorder Customization
    location_button := Array(277, 324, 4)
    opacity_button := Array(702, 353)
    honk_button := Array(224, 384)
    if (honk_button.Length < 2) {
        MsgBox("honk_button array must have at least 2 elements")
        ExitApp()
    }
    if (honk_button[1] < 0 || honk_button[1] > 1920) {
        MsgBox("honk_button X coordinate out of bounds: " . honk_button[1])
        ExitApp()
    }
    if (honk_button[2] < 0 || honk_button[2] > 1200) {
        MsgBox("honk_button Y coordinate out of bounds: " . honk_button[2])
        ExitApp()
    }
    honk_toggled := PixelGetColor(honk_button[1], honk_button[2])
    encoder_button := Array(293, 410, 3)
    settings_button := Array(456, 410)

    ; Upload Manager
    move_button := Array(651, 468)
    open_button := Array(706, 468)
    date_min_button := Array(258, 528)
    date_max_button := Array(404, 528)
    /* TODO: grid out calendar coordinates
    291-601 X
    638-747 Y */

    ; Upload Tracker
    collapse_button := Array(31, 681)
    if (collapse_button.Length < 2) {
        MsgBox("collapse_button array must have at least 2 elements")
        ExitApp()
    }
    if (collapse_button[1] < 0 || collapse_button[1] > 1920) {
        MsgBox("collapse_button X coordinate out of bounds: " . collapse_button[1])
        ExitApp()
    }
    if (collapse_button[2] < 0 || collapse_button[2] > 1200) {
        MsgBox("collapse_button Y coordinate out of bounds: " . collapse_button[2])
        ExitApp()
    }
    collapse_toggled := PixelGetColor(collapse_button[1], collapse_button[2])
    unreliable_button := Array(27, 887)
    after_button := Array(27, 913)
    upload_button := Array(383, 946)
    FAQ_button := Array(32, 1002)
    logs_button := Array(76, 1002)
    website_button := Array(689, 1002)
    return
}

if WinExist("ahk_exe OWL Control.exe") {
    WinActivate "ahk_exe OWL Control.exe"  ; Explicitly activate before waiting
    InitInputSettings()
    if (!WinWaitActive("ahk_exe OWL Control.exe", , 5)) {  ; 5 second timeout to prevent indefinite hang
        MsgBox("Failed to activate OWL Control window within 5 seconds")
        ExitApp()
    }
    InitUIVars()  ; Initialize UI vars AFTER window is confirmed active (PixelGetColor calls need visible window)
    if (reset_position.Length < 2) {
        MsgBox("reset_position array must have at least 2 elements")
        ExitApp()
    }
    if (reset_position[1] < 0 || reset_position[1] > 1920) {
        MsgBox("reset_position X coordinate out of bounds: " . reset_position[1])
        ExitApp()
    }
    if (reset_position[2] < 0 || reset_position[2] > 1200) {
        MsgBox("reset_position Y coordinate out of bounds: " . reset_position[2])
        ExitApp()
    }
    MouseClick "left", reset_position[1], reset_position[2]   ; Resets tabs to top of UI

    if (stop_toggle == checked_true) {
        y := stop_button[2] + option_offset
        if (y < 0 || y > 1200) {
            MsgBox("Calculated Y coordinate out of bounds: " . y)
            ExitApp()
        }
        x := stop_button[1]
        if (x < 0 || x > 1920) {
            MsgBox("Calculated X coordinate out of bounds: " . x)
            ExitApp()
        }
        MouseClick "left", x, y
    }

    if (collapse_toggled == collapse_true)
        MouseClick "left", collapse_button[1], collapse_button[2]

    ; dont click logout, hotkey, stop
    buttons := [location_button, opacity_button, honk_button, encoder_button,
        unreliable_button, after_button, upload_button, settings_button, date_min_button, date_max_button, logs_button,
        open_button, move_button, website_button, FAQ_button, collapse_button]

    for coords in buttons {
        if (coords.Length < 2) {
            MsgBox("Button coordinates array must have at least 2 elements")
            ExitApp()
        }
        x := coords[1]
        if (x < 0 || x > 1920) {
            MsgBox("Calculated X coordinate out of bounds: " . x)
            ExitApp()
        }
        if (coords.Length = 3) {
            repeat := coords[3]
            if (repeat < 1 || repeat > 100) {
                MsgBox("Invalid repeat count: must be between 1 and 100, got " . repeat)
                ExitApp()
            }
            loop repeat {
                y := coords[2]
                if (y < 0 || y > 1200) {
                    MsgBox("Calculated Y coordinate out of bounds in loop: " . y)
                    ExitApp()
                }
                MouseClick "left", x, y
                y := (coords[2] + (A_Index) * option_offset) - collapse_offset
                if (y < 0 || y > 1200) {
                    MsgBox("Calculated Y coordinate out of bounds in loop: " . y)
                    ExitApp()
                }
                MouseClick "left", x, y
            }
        } else {
            y := coords[2] - collapse_offset
            if (y < 0 || y > 1200) {
                MsgBox("Calculated Y coordinate out of bounds: " . y)
                ExitApp()
            }
            MouseClick "left", x, y
        }
    }
    WinMinimize "A"  ; Minimize window after all UI operations complete
} else {
    MsgBox("OWL Control.exe window not found - ensure application is running")
    ExitApp()
}

/*
legacy key tabbing, might be useful for autogenerating command
slower execution than clicking tho
pc_website := 5 ; Press count
pc_logs := 2
pc_faq := 3
pc_upload := 4
pc_after := 5
pc_reliable := 6
pc_tracker := 9
pc_date_max := 10
pc_date_min := 11
pc_move := 13
pc_open := 14
pc_settings :=15
pc_video :=16
pc_honk :=17
pc_opacity :=18
pc_slider :=19
pc_location :=20
pc_stop_toggle :=
press_count := pc_move
tc_logout := 1  ; tab count
tc_hotkey := 3
tab_count := tc_hotkey

Loop press_count {
Send "{Shift down}{Tab down}{Tab up}{Shift up}"
}
*/
