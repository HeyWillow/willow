#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "esp_wifi.h"
#include "lvgl.h"
#include "sdkconfig.h"
#include "slvgl.h"

#include "driver/spi_master.h"
#include "esp_eth.h"

#include "shared.h"

#define WILLOW_ETHERNET_CS        10
#define WILLOW_ETHERNET_INT       14
#define WILLOW_ETHERNET_RST       9
#define WILLOW_ETHERNET_MISO      13
#define WILLOW_ETHERNET_MOSI      11
#define WILLOW_ETHERNET_SCLK      12
#define WILLOW_ETHERNET_SPI_BUS   2
#define WILLOW_ETHERNET_SPI_SPEED 36 * 1000 * 1000
#define WILLOW_ETHERNET_PHY       1

#define INIT_SPI_ETH_MODULE_CONFIG(eth_module_config, num)                                                             \
    do {                                                                                                               \
        eth_module_config[num].spi_cs_gpio = WILLOW_ETHERNET_CS;                                                       \
        eth_module_config[num].int_gpio = WILLOW_ETHERNET_INT;                                                         \
        eth_module_config[num].phy_reset_gpio = WILLOW_ETHERNET_RST;                                                   \
        eth_module_config[num].phy_addr = WILLOW_ETHERNET_PHY;                                                         \
    } while (0)

static const char *TAG = "WILLOW/ETHERNET";

typedef struct {
    uint8_t spi_cs_gpio;
    uint8_t int_gpio;
    int8_t phy_reset_gpio;
    uint8_t phy_addr;
} spi_eth_module_config_t;

/** Event handler for Ethernet events */
static void eth_event_handler(void *arg, esp_event_base_t event_base, int32_t event_id, void *event_data)
{
    uint8_t mac_addr[6] = {0};
    /* we can get the ethernet driver handle from event data */
    esp_eth_handle_t eth_handle = *(esp_eth_handle_t *)event_data;

    switch (event_id) {
        case ETHERNET_EVENT_CONNECTED:
            esp_eth_ioctl(eth_handle, ETH_CMD_G_MAC_ADDR, mac_addr);
            ESP_LOGI(TAG, "Ethernet Link Up");
            ESP_LOGI(TAG, "Ethernet HW Addr %02x:%02x:%02x:%02x:%02x:%02x", mac_addr[0], mac_addr[1], mac_addr[2],
                     mac_addr[3], mac_addr[4], mac_addr[5]);
            break;
        case ETHERNET_EVENT_DISCONNECTED:
            ESP_LOGI(TAG, "Ethernet Link Down");
            break;
        case ETHERNET_EVENT_START:
            ESP_LOGI(TAG, "Ethernet Started");
            break;
        case ETHERNET_EVENT_STOP:
            ESP_LOGI(TAG, "Ethernet Stopped");
            break;
        default:
            break;
    }
}

/** Event handler for IP_EVENT_ETH_GOT_IP */
static void got_ip_event_handler(void *arg, esp_event_base_t event_base, int32_t event_id, void *event_data)
{
    ip_event_got_ip_t *event = (ip_event_got_ip_t *)event_data;
    const esp_netif_ip_info_t *ip_info = &event->ip_info;

    ESP_LOGI(TAG, "Ethernet Got IP Address");
    ESP_LOGI(TAG, "ETHIP: " IPSTR, IP2STR(&ip_info->ip));
    ESP_LOGI(TAG, "ETHMASK: " IPSTR, IP2STR(&ip_info->netmask));
    ESP_LOGI(TAG, "ETHGW: " IPSTR, IP2STR(&ip_info->gw));
}

esp_err_t init_ethernet(void)
{
    esp_err_t ret = ESP_OK;

    // Start Ethernet
    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_obj_clear_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_style_text_align(lbl_ln4, LV_TEXT_ALIGN_CENTER, 0);
        lv_label_set_text_static(lbl_ln4, "Connecting to Ethernet ...");
        lvgl_port_unlock();
    }

    // Create instance(s) of esp-netif for SPI Ethernet(s)
    esp_netif_inherent_config_t esp_netif_config = ESP_NETIF_INHERENT_DEFAULT_ETH();
    esp_netif_config_t cfg_spi = {.base = &esp_netif_config, .stack = ESP_NETIF_NETSTACK_DEFAULT_ETH};
    esp_netif_t *eth_netif_spi[1] = {NULL};
    char if_key_str[10];
    char if_desc_str[10];
    char num_str[3];
    for (int i = 0; i < 1; i++) {
        itoa(i, num_str, 10);
        strcat(strcpy(if_key_str, "ETH_SPI_"), num_str);
        strcat(strcpy(if_desc_str, "eth"), num_str);
        esp_netif_config.if_key = if_key_str;
        esp_netif_config.if_desc = if_desc_str;
        esp_netif_config.route_prio = 30 - i;
        eth_netif_spi[i] = esp_netif_new(&cfg_spi);
    }

    // Init MAC and PHY configs to default
    eth_mac_config_t mac_config_spi = ETH_MAC_DEFAULT_CONFIG();
    eth_phy_config_t phy_config_spi = ETH_PHY_DEFAULT_CONFIG();

    // Init SPI bus
    spi_bus_config_t buscfg = {
        .miso_io_num = WILLOW_ETHERNET_MISO,
        .mosi_io_num = WILLOW_ETHERNET_MOSI,
        .sclk_io_num = WILLOW_ETHERNET_SCLK,
        .quadwp_io_num = -1,
        .quadhd_io_num = -1,
    };
    ESP_ERROR_CHECK(spi_bus_initialize(WILLOW_ETHERNET_SPI_BUS, &buscfg, SPI_DMA_CH_AUTO));

    // Init specific SPI Ethernet module configuration from Kconfig (CS GPIO, Interrupt GPIO, etc.)
    spi_eth_module_config_t spi_eth_module_config[1];
    INIT_SPI_ETH_MODULE_CONFIG(spi_eth_module_config, 0);

    // Configure SPI interface and Ethernet driver for specific SPI module
    esp_eth_mac_t *mac_spi[1];
    esp_eth_phy_t *phy_spi[1];
    esp_eth_handle_t eth_handle_spi[1] = {NULL};

    // W5500
    spi_device_interface_config_t devcfg = {.command_bits = 16, // Actually it's the address phase in W5500 SPI frame
                                            .address_bits = 8,  // Actually it's the control phase in W5500 SPI frame
                                            .mode = 0,
                                            .clock_speed_hz = WILLOW_ETHERNET_SPI_SPEED,
                                            .queue_size = 20};

    for (int i = 0; i < 1; i++) {
        // Set SPI module Chip Select GPIO
        devcfg.spics_io_num = spi_eth_module_config[i].spi_cs_gpio;

        // w5500 ethernet driver is based on spi driver
        eth_w5500_config_t w5500_config = ETH_W5500_DEFAULT_CONFIG(WILLOW_ETHERNET_SPI_BUS, &devcfg);

        // Set remaining GPIO numbers and configuration used by the SPI module
        w5500_config.int_gpio_num = spi_eth_module_config[i].int_gpio;
        phy_config_spi.phy_addr = spi_eth_module_config[i].phy_addr;
        phy_config_spi.reset_gpio_num = spi_eth_module_config[i].phy_reset_gpio;

        mac_spi[i] = esp_eth_mac_new_w5500(&w5500_config, &mac_config_spi);
        phy_spi[i] = esp_eth_phy_new_w5500(&phy_config_spi);
    }

    for (int i = 0; i < 1; i++) {
        esp_eth_config_t eth_config_spi = ETH_DEFAULT_CONFIG(mac_spi[i], phy_spi[i]);
        ESP_ERROR_CHECK(esp_eth_driver_install(&eth_config_spi, &eth_handle_spi[i]));

        /* The SPI Ethernet module might not have a burned factory MAC address, we cat to set it manually.
       02:00:00 is a Locally Administered OUI range so should not be used except when testing on a LAN under your
       control.
        */
        ESP_ERROR_CHECK(
            esp_eth_ioctl(eth_handle_spi[i], ETH_CMD_S_MAC_ADDR, (uint8_t[]){0x02, 0x00, 0x00, 0x12, 0x34, 0x56 + i}));

        // attach Ethernet driver to TCP/IP stack
        ESP_ERROR_CHECK(esp_netif_attach(eth_netif_spi[i], esp_eth_new_netif_glue(eth_handle_spi[i])));
    }

    // Register user defined event handers
    ESP_ERROR_CHECK(esp_event_handler_register(ETH_EVENT, ESP_EVENT_ANY_ID, &eth_event_handler, NULL));
    ESP_ERROR_CHECK(esp_event_handler_register(IP_EVENT, IP_EVENT_ETH_GOT_IP, &got_ip_event_handler, NULL));

    for (int i = 0; i < 1; i++) {
        ESP_ERROR_CHECK(esp_eth_start(eth_handle_spi[i]));
    }

    for (int i = 0; i < 1; i++) {
        while (!esp_netif_is_netif_up(eth_netif_spi[i])) {
            ESP_LOGI(TAG, "Waiting on Ethernet...");
            vTaskDelay(500 / portTICK_PERIOD_MS);
        }
    }

    // HACK
    vTaskDelay(5000 / portTICK_PERIOD_MS);

    return ret;
}
