use std::convert::TryInto;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::filestruct;

use actix_files as fs;
use actix_multipart::Multipart;
use actix_web::{http::Method, web, App, Error, HttpResponse, HttpServer, Responder};
use futures::{StreamExt, TryStreamExt};
use rusqlite::NO_PARAMS;
use serde_json::Value;
use teloxide::prelude::*;
use teloxide::types::ChatId;
use teloxide::types::InputFile;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::io::{BufReader, BufWriter};
use webdav_handler::actix::*;
use webdav_handler::{fakels::FakeLs, localfs::LocalFs, DavConfig, DavHandler};

pub static mut CONFIG: Option<crate::config::Config> = None;
pub static mut CHANNEL: Option<Arc<Mutex<mpsc::Sender<crate::filestruct::SendedFile>>>> = None;
const MEGABYTES_50: u64 = 50000000;

async fn api(data: web::Json<Value>, info: web::Path<String>) -> impl Responder {
    let method = info.as_str();
    trace!("Got request, method: {} and params : {:?}", method, data);
    let mut response = format!("Method {} not found", method);
    // match method {
    //     "getFiles" => {
    //         let catalog = data["catalog"].as_str().unwrap_or("main");
    //         let files = filestruct::get_all_files(catalog);
    //         let string = serde_json::to_string(&files).unwrap();
    //         response = format!("{}", string);
    //     }
    //     "createCatalog" => {
    //         let catalog_name = data["catalog_name"]
    //             .as_str()
    //             .unwrap_or("new_catalog")
    //             .to_owned();
    //         let current_catalog = data["current_catalog"]
    //             .as_str()
    //             .unwrap_or("main")
    //             .to_owned();
    //         web::block(move || {
    //             Ok::<_, ()>(filestruct::make_catalog(current_catalog, catalog_name))
    //         })
    //         .await
    //         .unwrap();
    //         response = "OK".to_owned();
    //     }
    //     _ => {}
    // }

    response
}

async fn save_file(info: web::Path<String>, mut payload: Multipart) -> Result<HttpResponse, Error> {
    // iterate over multipart stream
    let catalog = info;
    info!("Someone wants to upload a file to catalog {}", catalog);
    let mut global_filepath = PathBuf::new();

    while let Ok(Some(mut field)) = payload.try_next().await {
        let content_type = field.content_disposition().unwrap();
        let filename = content_type.get_filename().unwrap();
        std::fs::create_dir("./tmp");
        let filepath = format!("./tmp/{}", filename);
        global_filepath = PathBuf::from(filepath.clone());
        // File::create is blocking operation, use threadpool
        let mut f = web::block(|| std::fs::File::create(filepath))
            .await
            .unwrap();
        // Field in turn is stream of *Bytes* object
        while let Some(chunk) = field.next().await {
            let data = chunk.unwrap();
            // filesystem operations are blocking, we have to use threadpool
            f = web::block(move || f.write_all(&data).map(|_| f)).await?;
        }
    }

    filestruct::upload_file(&catalog, global_filepath);
    Ok(HttpResponse::Ok().into())
}

// #[actix_rt::main]
pub async fn run(config: crate::config::Config) {
    filestruct::database_init();

    let (tx, rx) = mpsc::channel();

    unsafe {
        info!("Make a global channel");
        CHANNEL = Some(Arc::new(Mutex::new(tx)));
    };

    let port = config.server_port.clone();

    unsafe {
        CONFIG = Some(config);
    }

    let local = tokio::task::LocalSet::new();
    let sys = actix_rt::System::run_in_tokio("server", &local);

    tokio::spawn(async move {
        info!("Spawn local!");
        let token = unsafe { CONFIG.as_ref().unwrap().telegram_token.clone() };
        let chat_id = unsafe { CONFIG.as_ref().unwrap().telegram_chatid.clone() };

        info!("Token: {}", token);
        let bot = Bot::new(token);

        for mut info in rx {
            info!("Get something on channel");
            let bot_cloned = bot.clone();
            tokio::spawn(async move {
                info!("{:?}", info);
                let mut parts = String::from("0");
                let mut reader_file = File::open(info.file.clone()).await.unwrap();
                let filesize = reader_file.metadata().await.unwrap().len();
                info.info.size = filesize.try_into().unwrap();

                if filesize > MEGABYTES_50 {
                    let times_of_large_read = filesize / MEGABYTES_50;
                    let last_time_bytes_read = filesize - times_of_large_read * MEGABYTES_50;
                    info!(
                        "Read {} times, last time: {}",
                        times_of_large_read, last_time_bytes_read
                    );
                    parts = String::from("");
                    info!("File is bigger than 50MB, split it up");
                    let mut parts_ids = Vec::new();
                    let mut i: u64 = 0;
                    let mut reader = BufReader::new(reader_file);
                    loop {
                        let mut buff = Vec::new();
                        info!("{}", i);

                        if i > times_of_large_read {
                            break; // end
                        }
                        if i == times_of_large_read {
                            info!("Make a little read");
                            buff = vec![0; last_time_bytes_read.try_into().unwrap()];
                            let n = reader.read_exact(&mut buff);
                        } else {
                            info!("Make a huge read");
                            buff = vec![0; MEGABYTES_50.try_into().unwrap()];
                            let n = reader.read_exact(&mut buff);
                        }

                        let mut part_name = info.file.clone();
                        part_name.set_file_name(format!(
                            "{}_part_{}",
                            part_name.file_name().unwrap().to_str().unwrap(),
                            i
                        ));
                        info!("New filename: {:?}", part_name);
                        let mut writer = BufWriter::new(File::create(&part_name).await.unwrap());
                        writer.write_all(&buff).await.unwrap();
                        writer.flush().await.unwrap();
                        info!("Sending part with name: {:?}", part_name);
                        tokio::time::delay_for(std::time::Duration::from_secs(1)).await; // or else telegram will ban us
                        let msg = bot_cloned
                            .send_document(ChatId::Id(chat_id), InputFile::File(part_name.clone()))
                            .send()
                            .await
                            .unwrap();
                        parts_ids.push(msg.id);
                        i += 1;
                        tokio::fs::remove_file(part_name).await.unwrap();
                    }

                    info!("Splited, ids: {:#?}", parts_ids);

                    info.info.parts = parts_ids;
                } else {
                    info!("Small file, doesnt need to be splitted up");
                    info!("Sending document to chat {}", chat_id);
                    let msg = bot_cloned
                        .send_document(ChatId::Id(chat_id), InputFile::File(info.file.clone()))
                        .send()
                        .await
                        .unwrap();
                    info!("Sended! {:#?}", msg);
                    info.info.telegram_id = msg.id.try_into().unwrap();
                    info.info.parts = vec![0];
                }

                tokio::fs::remove_file(info.file.clone()).await.unwrap();

                let mut db = rusqlite::Connection::open("./database/files.db").unwrap();
                info.info.insert(&mut db); // insert catalog into current one

                info!("Saved to database!");
            })
            .await
            .unwrap();
        }
    });

    println!("Started on 127.0.0.1:{}", port);

    HttpServer::new(|| {
        let dav_server = DavHandler::builder()
            .filesystem(LocalFs::new(".", false, false, false))
            .locksystem(FakeLs::new())
            .build_handler();

        App::new()
            .data(dav_server.clone())
            .service(web::resource("/webdav{tail:.*}").to(crate::webdav::dav_handler))
            .service(fs::Files::new("/static", "./static").show_files_listing())
            .route("/api/{method}/", web::post().to(api))
            .route("/upload/{dir}/", web::post().to(save_file))
    })
    .bind(&format!("127.0.0.1:{}", port))
    .unwrap()
    .run()
    .await
    .unwrap();

    sys.await.unwrap();
}
