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
    if (logout_button[1] < 0 || logout_button[1] > 1920) {
        MsgBox("logout_button X coordinate out of bounds: " . logout_button[1])
        ExitApp()
    }
    if (logout_button[2] < 0 || logout_button[2] > 1080) {
        MsgBox("logout_button Y coordinate out of bounds: " . logout_button[2])
        ExitApp()
    }

    ;   Keyboard Shortcuts
    hotkey_button := Array(412, 186)
    if (hotkey_button.Length < 2) {
        MsgBox("hotkey_button array must have at least 2 elements")
        ExitApp()
    }
    if (hotkey_button[1] < 0 || hotkey_button[1] > 1920) {
        MsgBox("hotkey_button X coordinate out of bounds: " . hotkey_button[1])
        ExitApp()
    }
    if (hotkey_button[2] < 0 || hotkey_button[2] > 1080) {
        MsgBox("hotkey_button Y coordinate out of bounds: " . hotkey_button[2])
        ExitApp()
    }
    hotkey_stop := Array(412, 216)
    if (hotkey_stop.Length < 2) {
        MsgBox("hotkey_stop array must have at least 2 elements")
        ExitApp()
    }
    if (hotkey_stop[1] < 0 || hotkey_stop[1] > 1920) {
        MsgBox("hotkey_stop X coordinate out of bounds: " . hotkey_stop[1])
        ExitApp()
    }
    if (hotkey_stop[2] < 0 || hotkey_stop[2] > 1080) {
        MsgBox("hotkey_stop Y coordinate out of bounds: " . hotkey_stop[2])
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
    if (stop_button[2] < 0 || stop_button[2] > 1080) {
        MsgBox("stop_button Y coordinate out of bounds: " . stop_button[2])
        ExitApp()
    }
    stop_toggle := PixelGetColor(stop_button[1], stop_button[2])

    ;   Recorder Customization
    location_button := Array(277, 324, 4)
    if (location_button.Length < 2) {
        MsgBox("location_button array must have at least 2 elements")
        ExitApp()
    }
    if (location_button[1] < 0 || location_button[1] > 1920) {
        MsgBox("location_button X coordinate out of bounds: " . location_button[1])
        ExitApp()
    }
    if (location_button[2] < 0 || location_button[2] > 1080) {
        MsgBox("location_button Y coordinate out of bounds: " . location_button[2])
        ExitApp()
    }
    if (location_button.Length >= 3) {
        if (location_button[3] < 1 || location_button[3] > 100) {
            MsgBox("location_button option count out of bounds: " . location_button[3])
            ExitApp()
        }
    }
    opacity_button := Array(702, 353)
    if (opacity_button.Length < 2) {
        MsgBox("opacity_button array must have at least 2 elements")
        ExitApp()
    }
    if (opacity_button[1] < 0 || opacity_button[1] > 1920) {
        MsgBox("opacity_button X coordinate out of bounds: " . opacity_button[1])
        ExitApp()
    }
    if (opacity_button[2] < 0 || opacity_button[2] > 1080) {
        MsgBox("opacity_button Y coordinate out of bounds: " . opacity_button[2])
        ExitApp()
    }
    honk_button := Array(224, 384)
    if (honk_button.Length < 2) {
        MsgBox("honk_button array must have at least 2 elements")
        ExitApp()
    }
    if (honk_button[1] < 0 || honk_button[1] > 1920) {
        MsgBox("honk_button X coordinate out of bounds: " . honk_button[1])
        ExitApp()
    }
    if (honk_button[2] < 0 || honk_button[2] > 1080) {
        MsgBox("honk_button Y coordinate out of bounds: " . honk_button[2])
        ExitApp()
    }
    honk_toggled := PixelGetColor(honk_button[1], honk_button[2])
    encoder_button := Array(293, 410, 3)
    if (encoder_button.Length < 2) {
        MsgBox("encoder_button array must have at least 2 elements")
        ExitApp()
    }
    if (encoder_button[1] < 0 || encoder_button[1] > 1920) {
        MsgBox("encoder_button X coordinate out of bounds: " . encoder_button[1])
        ExitApp()
    }
    if (encoder_button[2] < 0 || encoder_button[2] > 1080) {
        MsgBox("encoder_button Y coordinate out of bounds: " . encoder_button[2])
        ExitApp()
    }
    if (encoder_button.Length >= 3) {
        if (encoder_button[3] < 1 || encoder_button[3] > 100) {
            MsgBox("encoder_button option count out of bounds: " . encoder_button[3])
            ExitApp()
        }
    }
    settings_button := Array(456, 410)
    if (settings_button.Length < 2) {
        MsgBox("settings_button array must have at least 2 elements")
        ExitApp()
    }
    if (settings_button[1] < 0 || settings_button[1] > 1920) {
        MsgBox("settings_button X coordinate out of bounds: " . settings_button[1])
        ExitApp()
    }
    if (settings_button[2] < 0 || settings_button[2] > 1080) {
        MsgBox("settings_button Y coordinate out of bounds: " . settings_button[2])
        ExitApp()
    }

    ; Upload Manager
    move_button := Array(651, 468)
    if (move_button.Length < 2) {
        MsgBox("move_button array must have at least 2 elements")
        ExitApp()
    }
    if (move_button[1] < 0 || move_button[1] > 1920) {
        MsgBox("move_button X coordinate out of bounds: " . move_button[1])
        ExitApp()
    }
    if (move_button[2] < 0 || move_button[2] > 1080) {
        MsgBox("move_button Y coordinate out of bounds: " . move_button[2])
        ExitApp()
    }
    if (move_button.Length >= 3) {
        if (move_button[3] < 1 || move_button[3] > 100) {
            MsgBox("move_button option count out of bounds: " . move_button[3])
            ExitApp()
        }
    }
    open_button := Array(706, 468)
    if (open_button.Length < 2) {
        MsgBox("open_button array must have at least 2 elements")
        ExitApp()
    }
    if (open_button[1] < 0 || open_button[1] > 1920) {
        MsgBox("open_button X coordinate out of bounds: " . open_button[1])
        ExitApp()
    }
    if (open_button[2] < 0 || open_button[2] > 1080) {
        MsgBox("open_button Y coordinate out of bounds: " . open_button[2])
        ExitApp()
    }
    date_min_button := Array(258, 528)
    if (date_min_button.Length < 2) {
        MsgBox("date_min_button array must have at least 2 elements")
        ExitApp()
    }
    if (date_min_button[1] < 0 || date_min_button[1] > 1920) {
        MsgBox("date_min_button X coordinate out of bounds: " . date_min_button[1])
        ExitApp()
    }
    if (date_min_button[2] < 0 || date_min_button[2] > 1080) {
        MsgBox("date_min_button Y coordinate out of bounds: " . date_min_button[2])
        ExitApp()
    }
    date_max_button := Array(404, 528)
    if (date_max_button.Length < 2) {
        MsgBox("date_max_button array must have at least 2 elements")
        ExitApp()
    }
    if (date_max_button[1] < 0 || date_max_button[1] > 1920) {
        MsgBox("date_max_button X coordinate out of bounds: " . date_max_button[1])
        ExitApp()
    }
    if (date_max_button[2] < 0 || date_max_button[2] > 1080) {
        MsgBox("date_max_button Y coordinate out of bounds: " . date_max_button[2])
        ExitApp()
    }
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
    if (collapse_button[2] < 0 || collapse_button[2] > 1080) {
        MsgBox("collapse_button Y coordinate out of bounds: " . collapse_button[2])
        ExitApp()
    }
    collapse_toggled := PixelGetColor(collapse_button[1], collapse_button[2])
    unreliable_button := Array(27, 887)
    if (unreliable_button.Length < 2) {
        MsgBox("unreliable_button array must have at least 2 elements")
        ExitApp()
    }
    if (unreliable_button[1] < 0 || unreliable_button[1] > 1920) {
        MsgBox("unreliable_button X coordinate out of bounds: " . unreliable_button[1])
        ExitApp()
    }
    if (unreliable_button[2] < 0 || unreliable_button[2] > 1080) {
        MsgBox("unreliable_button Y coordinate out of bounds: " . unreliable_button[2])
        ExitApp()
    }
    after_button := Array(27, 913)
    if (after_button.Length < 2) {
        MsgBox("after_button array must have at least 2 elements")
        ExitApp()
    }
    if (after_button[1] < 0 || after_button[1] > 1920) {
        MsgBox("after_button X coordinate out of bounds: " . after_button[1])
        ExitApp()
    }
    if (after_button[2] < 0 || after_button[2] > 1080) {
        MsgBox("after_button Y coordinate out of bounds: " . after_button[2])
        ExitApp()
    }
    upload_button := Array(383, 946)
    if (upload_button.Length < 2) {
        MsgBox("upload_button array must have at least 2 elements")
        ExitApp()
    }
    if (upload_button[1] < 0 || upload_button[1] > 1920) {
        MsgBox("upload_button X coordinate out of bounds: " . upload_button[1])
        ExitApp()
    }
    if (upload_button[2] < 0 || upload_button[2] > 1080) {
        MsgBox("upload_button Y coordinate out of bounds: " . upload_button[2])
        ExitApp()
    }
    FAQ_button := Array(32, 1002)
    if (FAQ_button.Length < 2) {
        MsgBox("FAQ_button array must have at least 2 elements")
        ExitApp()
    }
    if (FAQ_button[1] < 0 || FAQ_button[1] > 1920) {
        MsgBox("FAQ_button X coordinate out of bounds: " . FAQ_button[1])
        ExitApp()
    }
    if (FAQ_button[2] < 0 || FAQ_button[2] > 1080) {
        MsgBox("FAQ_button Y coordinate out of bounds: " . FAQ_button[2])
        ExitApp()
    }
    logs_button := Array(76, 1002)
    if (logs_button.Length < 2) {
        MsgBox("logs_button array must have at least 2 elements")
        ExitApp()
    }
    if (logs_button[1] < 0 || logs_button[1] > 1920) {
        MsgBox("logs_button X coordinate out of bounds: " . logs_button[1])
        ExitApp()
    }
    if (logs_button[2] < 0 || logs_button[2] > 1080) {
        MsgBox("logs_button Y coordinate out of bounds: " . logs_button[2])
        ExitApp()
    }
    website_button := Array(689, 1002)
    if (website_button.Length < 2) {
        MsgBox("website_button array must have at least 2 elements")
        ExitApp()
    }
    if (website_button[1] < 0 || website_button[1] > 1920) {
        MsgBox("website_button X coordinate out of bounds: " . website_button[1])
        ExitApp()
    }
    if (website_button[2] < 0 || website_button[2] > 1080) {
        MsgBox("website_button Y coordinate out of bounds: " . website_button[2])
        ExitApp()
    }
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
    if (reset_position[2] < 0 || reset_position[2] > 1080) {
        MsgBox("reset_position Y coordinate out of bounds: " . reset_position[2])
        ExitApp()
    }
    MouseClick "left", reset_position[1], reset_position[2]   ; Resets tabs to top of UI

    if (stop_toggle == checked_true) {
        y := stop_button[2] + option_offset
        if (y < 0 || y > 1080) {
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
        if (coords.Length >= 3) {
            repeat := coords[3]
            if (repeat < 1 || repeat > 100) {
                MsgBox("Invalid repeat count: must be between 1 and 100, got " . repeat)
                ExitApp()
            }
            loop repeat {
                y := coords[2]
                if (y < 0 || y > 1080) {
                    MsgBox("Calculated Y coordinate out of bounds in loop: " . y)
                    ExitApp()
                }
                MouseClick "left", x, y
                y := (coords[2] + (A_Index) * option_offset) - collapse_offset
                if (y < 0 || y > 1080) {
                    MsgBox("Calculated Y coordinate out of bounds in loop: " . y)
                    ExitApp()
                }
                MouseClick "left", x, y
            }
        } else {
            y := coords[2] - collapse_offset
            if (y < 0 || y > 1080) {
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
