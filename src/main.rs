/*
    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.
    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.
    You should have received a copy of the GNU General Public License
    along with this program.  If not, see <https://www.gnu.org/licenses/>.
*/

use std::{path::Path, process::Command};

use config::Config;
use tide::{prelude::*, Request, Response, http::StatusCode, log::warn};
use libmedium::{parse_hwmons,sensors::{Input, Sensor}};

mod config;
mod disks;

#[derive(Serialize)]
struct SystemInfo {
    hardware: String,
    system_name: String,
    os_version: Option<String>,
    kernel_ver: String,
    last_uadate: Option<String>,
    hostname: String,
    boot_time: String,
    cpu_cores_count: u32,
    cpu_load_avg: f64, // one minute
    mem_total: f64,
    mem_used: f64,
    swap_total: f64,
    swap_used: f64,
    disk: Vec<Disk>,
    temperature: Vec<Temprature>
}

#[derive(Serialize)]
struct Disk {
    mount: String,
    total: f64,
    available: f64
}

#[derive(Serialize)]
struct Temprature {
    label: String,
    temp: f64
}

#[async_std::main]
async fn main() -> tide::Result<()> {
    let conf = config::Config::generate();
    
    tide::log::start();
    let mut app = tide::with_state(conf.clone());

    if let Some(val) = conf.static_dir {
        let path = Path::new(&val);
        if path.exists() && path.is_dir() {
            app.at("").serve_dir(path.to_str().unwrap())?;    
        } else { warn!("Static Directory dosen't exists!") }
        
        let path = path.join("index.html");
        if path.exists() {
            app.at("/").serve_file(path.to_str().unwrap())?;
        }
    }

    app.at("/exec/:command").get(exec_cmd);
    app.at("/poweroff").get(poweroff);
    app.at("/reboot").get(reboot);
    app.at("/sysinfo").get(system_info);
    app.at("/cmdquery").get(cmd_query);
    app.listen(format!("{}:{}", conf.addr, conf.port)).await?;
    Ok(())
}

async fn poweroff(_: Request<Config>) -> tide::Result {
    async_std::task::spawn(async {
        async_std::task::sleep(std::time::Duration::from_secs(3)).await;
        Command::new("poweroff").spawn().expect("Failed to poweroff!");
    });
    Ok("Reqesting to poweroff. Please see green led for for activity".into())
}

async fn reboot(_: Request<Config>) -> tide::Result {
    async_std::task::spawn(async {
        async_std::task::sleep(std::time::Duration::from_secs(3)).await;
        Command::new("reboot").spawn().expect("Failed to reboot!");
    });
    Ok("Reqesting to Rebooting.".into())
}

async fn system_info(_: Request<Config>) -> tide::Result {
    let os = sys_info::linux_os_release().unwrap();
    
    let mut cpu_load_avg = 0.0;
    if let Ok(ld) = sys_info::loadavg() {
        cpu_load_avg = ld.one;
    }

    let mut mem_total = 0.0;
    let mut mem_used = 0.0;
    let mut swap_total = 0.0;
    let mut swap_used = 0.0;
    if let Ok(info) = sys_info::mem_info() {
        mem_total = info.total as f64 / 1024.0;
        mem_used = (info.total - info.free) as f64 / 1024.0;
        swap_total = info.swap_total as f64 / 1024.0;
        swap_used = (info.swap_total - info.swap_free) as f64 / 1024.0;
    }

    let mut disk = Vec::new();
    for d in disks::get_disks_info() {
        disk.push(Disk {
            mount: d.mount,
            total: d.total as f64 / 1048576.0, // bytes to mb
            available: d.available as f64 / 1048576.0 // bytes to mb
        });
    }

    let mut temperature: Vec<Temprature> = Vec::new();
    let hwmons = parse_hwmons().unwrap();
    for (_, _, hwmon) in &hwmons {
        for (_, temp_sensor) in hwmon.temps() {
            let tmp = temp_sensor.read_input().unwrap();
            temperature.push(Temprature {
                label: temp_sensor.name(),
                temp: tmp.as_degrees_celsius()
            });
            
        }
    }

    let boottime = std::time::Duration::from_secs(match  sys_info::boottime() {
        Ok(s) => s.tv_sec as u64,
        Err(_) => 0
    });

    let sys_info = SystemInfo {
        hardware: "".to_owned(),
        system_name: os.pretty_name.unwrap_or_default(),
        os_version: os.version,
        kernel_ver: sys_info::os_release().unwrap_or_default(),
        last_uadate: last_update(),
        hostname: sys_info::hostname().unwrap_or_default(),
        boot_time: humantime::format_duration(boottime).to_string(),
        cpu_cores_count: sys_info::cpu_num().unwrap_or_default(),
        cpu_load_avg,
        mem_total,
        mem_used,
        swap_total,
        swap_used,
        disk,
        temperature
    };

    Ok(json!(sys_info).to_string().into())
}

fn last_update() -> Option<String> {
    let mut cmd = std::process::Command::new("bash");
    cmd.args(&["-c", "grep 'pacman -Syu' /var/log/pacman.log | tail -n 1"]);

    let stdout = match cmd.output() {
        Ok(out) => out.stdout,
        Err(_) => return None
    };

    match String::from_utf8(stdout) {
        Ok(val) => {
            let s = val.split(" ").next()?;
            return Some(s[1..s.len()-1].to_owned());
        }, Err(_) => return None
    }
}

fn exec(cmd: &mut Command) -> String {
    let out = cmd.output();

    if out.is_err() {
        return "Failed to execute command!".to_owned();
    }
    
    match String::from_utf8(out.unwrap().stdout) {
        Ok(out) => return out,
        Err(_) => return "Request timeout".to_owned()
    }
}

async fn exec_cmd(req: Request<Config>) -> tide::Result {
    let cmd = req.state().commands.get(
        req.param("command").map_err(|e| {
            tide::log::error!("Error: {}", e);
            tide::Error::from_str(StatusCode::BadRequest, "Invalid Request!")
        })?
    ).ok_or_else(|| {
        tide::Error::from_str(StatusCode::BadRequest, "No such command!")
    })?.command.as_str();
    let args: Vec<_> = cmd.split_ascii_whitespace().collect();
    Ok(exec(Command::new(&args[0]).args(&args[1..])).into())
}

async fn cmd_query(req: Request<Config>) -> tide::Result {
    let body = tide::Body::from_json(&req.state().commands).map_err(|e| {
        tide::log::error!("Error: {}", e);
        tide::Error::from_str(StatusCode::ServiceUnavailable, "Internal server error!") 
    })?;
    let res = Response::builder(StatusCode::Ok).body(body).build();
    Ok(res)
}
