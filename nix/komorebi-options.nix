{ lib, ... }:
let
  animationStyle = (
    lib.types.oneOf [
      (lib.types.enum [
        "Linear"
        "EaseInSine"
        "EaseOutSine"
        "EaseInOutSine"
        "EaseInQuad"
        "EaseOutQuad"
        "EaseInOutQuad"
        "EaseInCubic"
        "EaseOutCubic"
        "EaseInOutCubic"
        "EaseInQuart"
        "EaseOutQuart"
        "EaseInOutQuart"
        "EaseInQuint"
        "EaseOutQuint"
        "EaseInOutQuint"
        "EaseInExpo"
        "EaseOutExpo"
        "EaseInOutExpo"
        "EaseInCirc"
        "EaseOutCirc"
        "EaseInOutCirc"
        "EaseInBack"
        "EaseOutBack"
        "EaseInOutBack"
        "EaseInElastic"
        "EaseOutElastic"
        "EaseInOutElastic"
        "EaseInBounce"
        "EaseOutBounce"
        "EaseInOutBounce"
      ])
      (lib.types.submodule {
        options = {
          CubicBezier = lib.mkOption {
            type = (lib.types.nullOr (lib.types.listOf lib.types.number));
            default = null;
          };
        };
      })
    ]
  );
  base16Value = (
    lib.types.enum [
      "Base00"
      "Base01"
      "Base02"
      "Base03"
      "Base04"
      "Base05"
      "Base06"
      "Base07"
      "Base08"
      "Base09"
      "Base0A"
      "Base0B"
      "Base0C"
      "Base0D"
      "Base0E"
      "Base0F"
    ]
  );
  catppuccinValue = (
    lib.types.enum [
      "Rosewater"
      "Flamingo"
      "Pink"
      "Mauve"
      "Red"
      "Maroon"
      "Peach"
      "Yellow"
      "Green"
      "Teal"
      "Sky"
      "Sapphire"
      "Blue"
      "Lavender"
      "Text"
      "Subtext1"
      "Subtext0"
      "Overlay2"
      "Overlay1"
      "Overlay0"
      "Surface2"
      "Surface1"
      "Surface0"
      "Base"
      "Mantle"
      "Crust"
    ]
  );
  colour = (
    lib.types.oneOf [
      (lib.types.submodule {
        options = {
          b = lib.mkOption {
            type = lib.types.int;
            description = "Blue";
          };
          g = lib.mkOption {
            type = lib.types.int;
            description = "Green";
          };
          r = lib.mkOption {
            type = lib.types.int;
            description = "Red";
          };
        };
      })
      lib.types.str
    ]
  );
  defaultLayout = (
    lib.types.enum [
      "BSP"
      "Columns"
      "Rows"
      "VerticalStack"
      "HorizontalStack"
      "UltrawideVerticalStack"
      "Grid"
      "RightMainVerticalStack"
      "Scrolling"
    ]
  );
  floatingLayerBehaviour = (
    lib.types.enum [
      "Tile"
      "Float"
    ]
  );
  idWithIdentifier = (
    lib.types.submodule {
      options = {
        id = lib.mkOption {
          type = lib.types.str;
        };
        kind = lib.mkOption {
          type = (
            lib.types.enum [
              "Exe"
              "Class"
              "Title"
              "Path"
            ]
          );
        };
        matching_strategy = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.enum [
                "Legacy"
                "Equals"
                "StartsWith"
                "EndsWith"
                "Contains"
                "Regex"
                "DoesNotEndWith"
                "DoesNotStartWith"
                "DoesNotEqual"
                "DoesNotContain"
              ]
            )
          );
          default = null;
        };
      };
    }
  );
  matchingRule = (
    lib.types.oneOf [
      idWithIdentifier
      (lib.types.listOf idWithIdentifier)
    ]
  );
  pathBuf = lib.types.str;
  placement = (
    lib.types.enum [
      "None"
      "Center"
      "CenterAndResize"
    ]
  );
  rect = (
    lib.types.submodule {
      options = {
        bottom = lib.mkOption {
          type = lib.types.int;
        };
        left = lib.mkOption {
          type = lib.types.int;
        };
        right = lib.mkOption {
          type = lib.types.int;
        };
        top = lib.mkOption {
          type = lib.types.int;
        };
      };
    }
  );
  wallpaper = (
    lib.types.submodule {
      options = {
        generate_theme = lib.mkOption {
          type = (lib.types.nullOr lib.types.bool);
          default = null;
          description = "Generate and apply Base16 theme for this wallpaper (default: true)";
        };
        path = lib.mkOption {
          type = pathBuf;
          description = "Path to the wallpaper image file";
        };
        theme_options = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.submodule {
                options = {
                  bar_accent = lib.mkOption {
                    type = (lib.types.nullOr base16Value);
                    default = null;
                    description = "Komorebi status bar accent (default: Base0D)";
                  };
                  floating_border = lib.mkOption {
                    type = (lib.types.nullOr base16Value);
                    default = null;
                    description = "Border colour when the window is floating (default: Base09)";
                  };
                  monocle_border = lib.mkOption {
                    type = (lib.types.nullOr base16Value);
                    default = null;
                    description = "Border colour when the container is in monocle mode (default: Base0F)";
                  };
                  single_border = lib.mkOption {
                    type = (lib.types.nullOr base16Value);
                    default = null;
                    description = "Border colour when the container contains a single window (default: Base0D)";
                  };
                  stack_border = lib.mkOption {
                    type = (lib.types.nullOr base16Value);
                    default = null;
                    description = "Border colour when the container contains multiple windows (default: Base0B)";
                  };
                  theme_variant = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.enum [
                          "Dark"
                          "Light"
                        ]
                      )
                    );
                    default = null;
                    description = "Specify Light or Dark variant for theme generation (default: Dark)";
                  };
                  unfocused_border = lib.mkOption {
                    type = (lib.types.nullOr base16Value);
                    default = null;
                    description = "Border colour when the container is unfocused (default: Base01)";
                  };
                  unfocused_locked_border = lib.mkOption {
                    type = (lib.types.nullOr base16Value);
                    default = null;
                    description = "Border colour when the container is unfocused and locked (default: Base08)";
                  };
                };
              }
            )
          );
          default = null;
          description = "Specify Light or Dark variant for theme generation (default: Dark)";
        };
      };
    }
  );
  windowContainerBehaviour = (
    lib.types.enum [
      "Create"
      "Append"
    ]
  );
in
{
  options.komorebi = lib.mkOption {
    type = lib.types.submodule {
      options = {
        animation = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.submodule {
                options = {
                  duration = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.attrsOf lib.types.int)
                          lib.types.int
                        ]
                      )
                    );
                    default = null;
                    description = "Set the animation duration in ms (default: 250)";
                  };
                  enabled = lib.mkOption {
                    type = (
                      lib.types.oneOf [
                        (lib.types.attrsOf lib.types.bool)
                        lib.types.bool
                      ]
                    );
                    description = "Enable or disable animations (default: false)";
                  };
                  fps = lib.mkOption {
                    type = (lib.types.nullOr lib.types.int);
                    default = null;
                    description = "Set the animation FPS (default: 60)";
                  };
                  style = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.attrsOf animationStyle)
                          animationStyle
                        ]
                      )
                    );
                    default = null;
                    description = "Set the animation style (default: Linear)";
                  };
                };
              }
            )
          );
          default = null;
          description = "Animations configuration options";
        };
        app_specific_configuration_path = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.oneOf [
                pathBuf
                (lib.types.listOf pathBuf)
              ]
            )
          );
          default = null;
          description = "Path to applications.json from komorebi-application-specific-configurations (default: None)";
        };
        border = lib.mkOption {
          type = (lib.types.nullOr lib.types.bool);
          default = null;
          description = "Display window borders (default: true)";
        };
        border_colours = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.submodule {
                options = {
                  floating = lib.mkOption {
                    type = (lib.types.nullOr colour);
                    default = null;
                    description = "Border colour when the container is in floating mode";
                  };
                  monocle = lib.mkOption {
                    type = (lib.types.nullOr colour);
                    default = null;
                    description = "Border colour when the container is in monocle mode";
                  };
                  single = lib.mkOption {
                    type = (lib.types.nullOr colour);
                    default = null;
                    description = "Border colour when the container contains a single window";
                  };
                  stack = lib.mkOption {
                    type = (lib.types.nullOr colour);
                    default = null;
                    description = "Border colour when the container contains multiple windows";
                  };
                  unfocused = lib.mkOption {
                    type = (lib.types.nullOr colour);
                    default = null;
                    description = "Border colour when the container is unfocused";
                  };
                  unfocused_locked = lib.mkOption {
                    type = (lib.types.nullOr colour);
                    default = null;
                    description = "Border colour when the container is unfocused and locked";
                  };
                };
              }
            )
          );
          default = null;
          description = "Window border colours for different container types (has no effect if a theme is defined)";
        };
        border_offset = lib.mkOption {
          type = (lib.types.nullOr lib.types.int);
          default = null;
          description = "Offset of window borders (default: 5)";
        };
        border_overflow_applications = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf matchingRule));
          default = null;
          description = "Identify border overflow applications";
        };
        border_radius = lib.mkOption {
          type = (lib.types.nullOr lib.types.int);
          default = null;
          description = "Radius of window borders (default: 10)";
        };
        border_width = lib.mkOption {
          type = (lib.types.nullOr lib.types.int);
          default = null;
          description = "Width of window borders (default: 6)";
        };
        cross_boundary_behaviour = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.enum [
                "Workspace"
                "Monitor"
              ]
            )
          );
          default = null;
          description = "Determine what happens when an action is called on a window at a monitor boundary (default: Monitor)";
        };
        cross_monitor_move_behaviour = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.enum [
                "Swap"
                "Insert"
                "NoOp"
              ]
            )
          );
          default = null;
          description = "Determine what happens when a window is moved across a monitor boundary (default: Swap)";
        };
        default_container_padding = lib.mkOption {
          type = (lib.types.nullOr lib.types.int);
          default = null;
          description = "Global default container padding (default: 10)";
        };
        default_workspace_padding = lib.mkOption {
          type = (lib.types.nullOr lib.types.int);
          default = null;
          description = "Add transparency to unfocused windows (default: false)\nGlobal default workspace padding (default: 10)";
        };
        display_index_preferences = lib.mkOption {
          type = (lib.types.nullOr (lib.types.attrsOf lib.types.str));
          default = null;
          description = "Set display index preferences";
        };
        float_override = lib.mkOption {
          type = (lib.types.nullOr lib.types.bool);
          default = null;
          description = "Enable or disable float override, which makes it so every new window opens in floating mode\n(default: false)";
        };
        float_override_placement = lib.mkOption {
          type = (lib.types.nullOr placement);
          default = null;
          description = "Determines the `Placement` to be used when spawning a window with float override active\n(default: None)";
        };
        float_rule_placement = lib.mkOption {
          type = (lib.types.nullOr placement);
          default = null;
          description = "Determines the `Placement` to be used when spawning a window that matches a\n'floating_applications' rule (default: None)";
        };
        floating_applications = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf matchingRule));
          default = null;
          description = "Identify applications which should be managed as floating windows";
        };
        floating_layer_behaviour = lib.mkOption {
          type = (lib.types.nullOr floatingLayerBehaviour);
          default = null;
          description = "Determines what happens on a new window when on the `FloatingLayer`\n(default: Tile)";
        };
        floating_layer_placement = lib.mkOption {
          type = (lib.types.nullOr placement);
          default = null;
          description = "Determines the `Placement` to be used when spawning a window on the floating layer with the\n`FloatingLayerBehaviour` set to `FloatingLayerBehaviour::Float` (default: Center)";
        };
        floating_window_aspect_ratio = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.oneOf [
                (lib.types.enum [
                  "Ultrawide"
                  "Widescreen"
                  "Standard"
                ])
                (lib.types.listOf lib.types.int)
              ]
            )
          );
          default = null;
          description = "Identify applications which are slow to send initial event notifications\nAspect ratio to resize with when toggling floating mode for a window";
        };
        global_work_area_offset = lib.mkOption {
          type = (lib.types.nullOr rect);
          default = null;
          description = "Global work area (space used for tiling) offset (default: None)";
        };
        ignore_rules = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf matchingRule));
          default = null;
          description = "Individual window floating rules";
        };
        layered_applications = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf matchingRule));
          default = null;
          description = "Identify applications that have the WS_EX_LAYERED extended window style";
        };
        manage_rules = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf matchingRule));
          default = null;
          description = "Individual window force-manage rules";
        };
        monitors = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.listOf (
                lib.types.submodule {
                  options = {
                    container_padding = lib.mkOption {
                      type = (lib.types.nullOr lib.types.int);
                      default = null;
                      description = "Container padding (default: global)";
                    };
                    floating_layer_behaviour = lib.mkOption {
                      type = (lib.types.nullOr floatingLayerBehaviour);
                      default = null;
                      description = "Determine what happens to a new window when the Floating workspace layer is active (default: Tile)";
                    };
                    wallpaper = lib.mkOption {
                      type = (lib.types.nullOr wallpaper);
                      default = null;
                      description = "Specify a wallpaper for this monitor";
                    };
                    window_based_work_area_offset = lib.mkOption {
                      type = (lib.types.nullOr rect);
                      default = null;
                      description = "Window based work area offset (default: None)";
                    };
                    window_based_work_area_offset_limit = lib.mkOption {
                      type = (lib.types.nullOr lib.types.int);
                      default = null;
                      description = "Open window limit after which the window based work area offset will no longer be applied (default: 1)";
                    };
                    window_hiding_position = lib.mkOption {
                      type = (
                        lib.types.nullOr (
                          lib.types.enum [
                            "BottomLeft"
                            "BottomRight"
                          ]
                        )
                      );
                      default = null;
                      description = "Determine which position windows should be hidden at on this monitor";
                    };
                    work_area_offset = lib.mkOption {
                      type = (lib.types.nullOr rect);
                      default = null;
                      description = "Monitor-specific work area offset (default: None)";
                    };
                    workspace_padding = lib.mkOption {
                      type = (lib.types.nullOr lib.types.int);
                      default = null;
                      description = "Workspace padding (default: global)";
                    };
                    workspaces = lib.mkOption {
                      type = (
                        lib.types.listOf (
                          lib.types.submodule {
                            options = {
                              apply_window_based_work_area_offset = lib.mkOption {
                                type = (lib.types.nullOr lib.types.bool);
                                default = null;
                                description = "Apply this monitor's window-based work area offset (default: true)";
                              };
                              container_padding = lib.mkOption {
                                type = (lib.types.nullOr lib.types.int);
                                default = null;
                                description = "Container padding (default: global)";
                              };
                              float_override = lib.mkOption {
                                type = (lib.types.nullOr lib.types.bool);
                                default = null;
                                description = "Enable or disable float override, which makes it so every new window opens in floating mode (default: false)";
                              };
                              floating_layer_behaviour = lib.mkOption {
                                type = (lib.types.nullOr floatingLayerBehaviour);
                                default = null;
                                description = "Determine what happens to a new window when the Floating workspace layer is active (default: Tile)";
                              };
                              initial_workspace_rules = lib.mkOption {
                                type = (lib.types.nullOr (lib.types.listOf matchingRule));
                                default = null;
                                description = "Initial workspace application rules";
                              };
                              layout = lib.mkOption {
                                type = (lib.types.nullOr defaultLayout);
                                default = null;
                                description = "Layout (default: BSP)";
                              };
                              layout_flip = lib.mkOption {
                                type = (
                                  lib.types.nullOr (
                                    lib.types.enum [
                                      "Horizontal"
                                      "Vertical"
                                      "HorizontalAndVertical"
                                    ]
                                  )
                                );
                                default = null;
                                description = "Specify an axis on which to flip the selected layout (default: None)";
                              };
                              layout_options = lib.mkOption {
                                type = (
                                  lib.types.nullOr (
                                    lib.types.submodule {
                                      options = {
                                        grid = lib.mkOption {
                                          type = (
                                            lib.types.nullOr (
                                              lib.types.submodule {
                                                options = {
                                                  rows = lib.mkOption {
                                                    type = lib.types.int;
                                                    description = "Maximum number of rows per grid column";
                                                  };
                                                };
                                              }
                                            )
                                          );
                                          default = null;
                                          description = "Options related to the Grid layout";
                                        };
                                        scrolling = lib.mkOption {
                                          type = (
                                            lib.types.nullOr (
                                              lib.types.submodule {
                                                options = {
                                                  center_focused_column = lib.mkOption {
                                                    type = (lib.types.nullOr lib.types.bool);
                                                    default = null;
                                                    description = "With an odd number of visible columns, keep the focused window column centered";
                                                  };
                                                  columns = lib.mkOption {
                                                    type = lib.types.int;
                                                    description = "Desired number of visible columns (default: 3)";
                                                  };
                                                };
                                              }
                                            )
                                          );
                                          default = null;
                                          description = "Options related to the Scrolling layout";
                                        };
                                      };
                                    }
                                  )
                                );
                                default = null;
                                description = "Layout-specific options (default: None)";
                              };
                              layout_rules = lib.mkOption {
                                type = (lib.types.nullOr (lib.types.attrsOf defaultLayout));
                                default = null;
                                description = "Layout rules in the format of threshold => layout (default: None)";
                              };
                              name = lib.mkOption {
                                type = lib.types.str;
                                description = "Name";
                              };
                              tile = lib.mkOption {
                                type = (lib.types.nullOr lib.types.bool);
                                default = null;
                                description = "Enable or disable tiling for the workspace (default: true)";
                              };
                              wallpaper = lib.mkOption {
                                type = (lib.types.nullOr wallpaper);
                                default = null;
                                description = "Specify a wallpaper for this workspace";
                              };
                              window_container_behaviour = lib.mkOption {
                                type = (lib.types.nullOr windowContainerBehaviour);
                                default = null;
                                description = "Determine what happens when a new window is opened (default: Create)";
                              };
                              window_container_behaviour_rules = lib.mkOption {
                                type = (lib.types.nullOr (lib.types.attrsOf windowContainerBehaviour));
                                default = null;
                                description = "Window container behaviour rules in the format of threshold => behaviour (default: None)";
                              };
                              work_area_offset = lib.mkOption {
                                type = (lib.types.nullOr rect);
                                default = null;
                                description = "Workspace specific work area offset (default: None)";
                              };
                              workspace_padding = lib.mkOption {
                                type = (lib.types.nullOr lib.types.int);
                                default = null;
                                description = "Workspace padding (default: global)";
                              };
                              workspace_rules = lib.mkOption {
                                type = (lib.types.nullOr (lib.types.listOf matchingRule));
                                default = null;
                                description = "Permanent workspace application rules";
                              };
                            };
                          }
                        )
                      );
                      description = "Workspace configurations";
                    };
                  };
                }
              )
            )
          );
          default = null;
          description = "Monitor and workspace configurations";
        };
        mouse_follows_focus = lib.mkOption {
          type = (lib.types.nullOr lib.types.bool);
          default = null;
          description = "Enable or disable mouse follows focus (default: true)";
        };
        object_name_change_applications = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf matchingRule));
          default = null;
          description = "Identify applications that send EVENT_OBJECT_NAMECHANGE on launch (very rare)";
        };
        object_name_change_title_ignore_list = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf lib.types.str));
          default = null;
          description = "Do not process EVENT_OBJECT_NAMECHANGE events as Show events for identified applications matching these title regexes";
        };
        resize_delta = lib.mkOption {
          type = (lib.types.nullOr lib.types.int);
          default = null;
          description = "Delta to resize windows by (default 50)";
        };
        theme = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.submodule {
                options = {
                  palette = lib.mkOption {
                    type = (
                      lib.types.enum [
                        "Catppuccin"
                        "Base16"
                        "Custom"
                      ]
                    );
                  };
                  unfocused_locked_border = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.nullOr catppuccinValue)
                          (lib.types.nullOr base16Value)
                        ]
                      )
                    );
                    default = null;
                  };
                  colours = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.submodule {
                          options = {
                            base_00 = lib.mkOption {
                              type = colour;
                            };
                            base_01 = lib.mkOption {
                              type = colour;
                            };
                            base_02 = lib.mkOption {
                              type = colour;
                            };
                            base_03 = lib.mkOption {
                              type = colour;
                            };
                            base_04 = lib.mkOption {
                              type = colour;
                            };
                            base_05 = lib.mkOption {
                              type = colour;
                            };
                            base_06 = lib.mkOption {
                              type = colour;
                            };
                            base_07 = lib.mkOption {
                              type = colour;
                            };
                            base_08 = lib.mkOption {
                              type = colour;
                            };
                            base_09 = lib.mkOption {
                              type = colour;
                            };
                            base_0a = lib.mkOption {
                              type = colour;
                            };
                            base_0b = lib.mkOption {
                              type = colour;
                            };
                            base_0c = lib.mkOption {
                              type = colour;
                            };
                            base_0d = lib.mkOption {
                              type = colour;
                            };
                            base_0e = lib.mkOption {
                              type = colour;
                            };
                            base_0f = lib.mkOption {
                              type = colour;
                            };
                          };
                        }
                      )
                    );
                    default = null;
                  };
                  floating_border = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.nullOr catppuccinValue)
                          (lib.types.nullOr base16Value)
                        ]
                      )
                    );
                    default = null;
                  };
                  unfocused_border = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.nullOr catppuccinValue)
                          (lib.types.nullOr base16Value)
                        ]
                      )
                    );
                    default = null;
                  };
                  stack_border = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.nullOr catppuccinValue)
                          (lib.types.nullOr base16Value)
                        ]
                      )
                    );
                    default = null;
                  };
                  single_border = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.nullOr catppuccinValue)
                          (lib.types.nullOr base16Value)
                        ]
                      )
                    );
                    default = null;
                  };
                  bar_accent = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.nullOr catppuccinValue)
                          (lib.types.nullOr base16Value)
                        ]
                      )
                    );
                    default = null;
                  };
                  name = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.enum [
                            "Frappe"
                            "Latte"
                            "Macchiato"
                            "Mocha"
                          ])
                          (lib.types.enum [
                            "3024"
                            "Apathy"
                            "Apprentice"
                            "Ashes"
                            "AtelierCaveLight"
                            "AtelierCave"
                            "AtelierDuneLight"
                            "AtelierDune"
                            "AtelierEstuaryLight"
                            "AtelierEstuary"
                            "AtelierForestLight"
                            "AtelierForest"
                            "AtelierHeathLight"
                            "AtelierHeath"
                            "AtelierLakesideLight"
                            "AtelierLakeside"
                            "AtelierPlateauLight"
                            "AtelierPlateau"
                            "AtelierSavannaLight"
                            "AtelierSavanna"
                            "AtelierSeasideLight"
                            "AtelierSeaside"
                            "AtelierSulphurpoolLight"
                            "AtelierSulphurpool"
                            "Atlas"
                            "AyuDark"
                            "AyuLight"
                            "AyuMirage"
                            "Aztec"
                            "Bespin"
                            "BlackMetalBathory"
                            "BlackMetalBurzum"
                            "BlackMetalDarkFuneral"
                            "BlackMetalGorgoroth"
                            "BlackMetalImmortal"
                            "BlackMetalKhold"
                            "BlackMetalMarduk"
                            "BlackMetalMayhem"
                            "BlackMetalNile"
                            "BlackMetalVenom"
                            "BlackMetal"
                            "Blueforest"
                            "Blueish"
                            "Brewer"
                            "Bright"
                            "Brogrammer"
                            "BrushtreesDark"
                            "Brushtrees"
                            "Caroline"
                            "CatppuccinFrappe"
                            "CatppuccinLatte"
                            "CatppuccinMacchiato"
                            "CatppuccinMocha"
                            "Chalk"
                            "Circus"
                            "ClassicDark"
                            "ClassicLight"
                            "Codeschool"
                            "Colors"
                            "Cupcake"
                            "Cupertino"
                            "DaOneBlack"
                            "DaOneGray"
                            "DaOneOcean"
                            "DaOnePaper"
                            "DaOneSea"
                            "DaOneWhite"
                            "DanqingLight"
                            "Danqing"
                            "Darcula"
                            "Darkmoss"
                            "Darktooth"
                            "Darkviolet"
                            "Decaf"
                            "DefaultDark"
                            "DefaultLight"
                            "Dirtysea"
                            "Dracula"
                            "EdgeDark"
                            "EdgeLight"
                            "Eighties"
                            "EmbersLight"
                            "Embers"
                            "Emil"
                            "EquilibriumDark"
                            "EquilibriumGrayDark"
                            "EquilibriumGrayLight"
                            "EquilibriumLight"
                            "Eris"
                            "Espresso"
                            "EvaDim"
                            "Eva"
                            "EvenokDark"
                            "EverforestDarkHard"
                            "Everforest"
                            "Flat"
                            "Framer"
                            "FruitSoda"
                            "Gigavolt"
                            "Github"
                            "GoogleDark"
                            "GoogleLight"
                            "Gotham"
                            "GrayscaleDark"
                            "GrayscaleLight"
                            "Greenscreen"
                            "Gruber"
                            "GruvboxDarkHard"
                            "GruvboxDarkMedium"
                            "GruvboxDarkPale"
                            "GruvboxDarkSoft"
                            "GruvboxLightHard"
                            "GruvboxLightMedium"
                            "GruvboxLightSoft"
                            "GruvboxMaterialDarkHard"
                            "GruvboxMaterialDarkMedium"
                            "GruvboxMaterialDarkSoft"
                            "GruvboxMaterialLightHard"
                            "GruvboxMaterialLightMedium"
                            "GruvboxMaterialLightSoft"
                            "Hardcore"
                            "Harmonic16Dark"
                            "Harmonic16Light"
                            "HeetchLight"
                            "Heetch"
                            "Helios"
                            "Hopscotch"
                            "HorizonDark"
                            "HorizonLight"
                            "HorizonTerminalDark"
                            "HorizonTerminalLight"
                            "HumanoidDark"
                            "HumanoidLight"
                            "IaDark"
                            "IaLight"
                            "Icy"
                            "Irblack"
                            "Isotope"
                            "Jabuti"
                            "Kanagawa"
                            "Katy"
                            "Kimber"
                            "Lime"
                            "Macintosh"
                            "Marrakesh"
                            "Materia"
                            "MaterialDarker"
                            "MaterialLighter"
                            "MaterialPalenight"
                            "MaterialVivid"
                            "Material"
                            "MeasuredDark"
                            "MeasuredLight"
                            "MellowPurple"
                            "MexicoLight"
                            "Mocha"
                            "Monokai"
                            "Moonlight"
                            "Mountain"
                            "Nebula"
                            "NordLight"
                            "Nord"
                            "Nova"
                            "Ocean"
                            "Oceanicnext"
                            "OneLight"
                            "OnedarkDark"
                            "Onedark"
                            "OutrunDark"
                            "OxocarbonDark"
                            "OxocarbonLight"
                            "Pandora"
                            "PapercolorDark"
                            "PapercolorLight"
                            "Paraiso"
                            "Pasque"
                            "Phd"
                            "Pico"
                            "Pinky"
                            "Pop"
                            "Porple"
                            "PreciousDarkEleven"
                            "PreciousDarkFifteen"
                            "PreciousLightWarm"
                            "PreciousLightWhite"
                            "PrimerDarkDimmed"
                            "PrimerDark"
                            "PrimerLight"
                            "Purpledream"
                            "Qualia"
                            "Railscasts"
                            "Rebecca"
                            "RosePineDawn"
                            "RosePineMoon"
                            "RosePine"
                            "Saga"
                            "Sagelight"
                            "Sakura"
                            "Sandcastle"
                            "SelenizedBlack"
                            "SelenizedDark"
                            "SelenizedLight"
                            "SelenizedWhite"
                            "Seti"
                            "ShadesOfPurple"
                            "ShadesmearDark"
                            "ShadesmearLight"
                            "Shapeshifter"
                            "SilkDark"
                            "SilkLight"
                            "Snazzy"
                            "SolarflareLight"
                            "Solarflare"
                            "SolarizedDark"
                            "SolarizedLight"
                            "Spaceduck"
                            "Spacemacs"
                            "Sparky"
                            "StandardizedDark"
                            "StandardizedLight"
                            "Stella"
                            "StillAlive"
                            "Summercamp"
                            "SummerfruitDark"
                            "SummerfruitLight"
                            "SynthMidnightDark"
                            "SynthMidnightLight"
                            "Tango"
                            "Tarot"
                            "Tender"
                            "TerracottaDark"
                            "Terracotta"
                            "TokyoCityDark"
                            "TokyoCityLight"
                            "TokyoCityTerminalDark"
                            "TokyoCityTerminalLight"
                            "TokyoNightDark"
                            "TokyoNightLight"
                            "TokyoNightMoon"
                            "TokyoNightStorm"
                            "TokyoNightTerminalDark"
                            "TokyoNightTerminalLight"
                            "TokyoNightTerminalStorm"
                            "TokyodarkTerminal"
                            "Tokyodark"
                            "TomorrowNightEighties"
                            "TomorrowNight"
                            "Tomorrow"
                            "Tube"
                            "Twilight"
                            "UnikittyDark"
                            "UnikittyLight"
                            "UnikittyReversible"
                            "Uwunicorn"
                            "Vesper"
                            "Vice"
                            "Vulcan"
                            "Windows10Light"
                            "Windows10"
                            "Windows95Light"
                            "Windows95"
                            "WindowsHighcontrastLight"
                            "WindowsHighcontrast"
                            "WindowsNtLight"
                            "WindowsNt"
                            "Woodland"
                            "XcodeDusk"
                            "Zenbones"
                            "Zenburn"
                          ])
                        ]
                      )
                    );
                    default = null;
                  };
                  monocle_border = lib.mkOption {
                    type = (
                      lib.types.nullOr (
                        lib.types.oneOf [
                          (lib.types.nullOr catppuccinValue)
                          (lib.types.nullOr base16Value)
                        ]
                      )
                    );
                    default = null;
                  };
                };
              }
            )
          );
          default = null;
          description = "Theme configuration options";
        };
        toggle_float_placement = lib.mkOption {
          type = (lib.types.nullOr placement);
          default = null;
          description = "Determines the placement of a new window when toggling to float (default: CenterAndResize)";
        };
        tray_and_multi_window_applications = lib.mkOption {
          type = (lib.types.nullOr (lib.types.listOf matchingRule));
          default = null;
          description = "Identify tray and multi-window applications";
        };
        unmanaged_window_operation_behaviour = lib.mkOption {
          type = (
            lib.types.nullOr (
              lib.types.enum [
                "Op"
                "NoOp"
              ]
            )
          );
          default = null;
          description = "Determine what happens when commands are sent while an unmanaged window is in the foreground (default: Op)";
        };
        window_container_behaviour = lib.mkOption {
          type = (lib.types.nullOr windowContainerBehaviour);
          default = null;
          description = "Determine what happens when a new window is opened (default: Create)";
        };
      };
    };
    default = { };
    description = "komorebi for Mac configuration";
  };
}
